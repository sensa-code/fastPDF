use std::path::{Path, PathBuf};
use std::sync::Arc;

use fastpdf_core::loader::LoadStrategy;
use fastpdf_core::positions::{FileIdentity, PositionRecord};
use fastpdf_core::{SavedView, SavedZoom, SessionConfig, ZoomMode};
use fastpdf_engine_api::{
    CancelToken, DocumentSource, EngineCapabilities, EngineDocument, EngineError, EngineInfo,
    OpenOptions, PageIndex, PageInfo, PageSize, PixmapMut, RenderOutcome, RenderRequest, Rotation,
    SharedBytes,
};

use super::*;
use crate::document::LockedSource;

/// Documents of as many Letter pages as the file has bytes.
struct Engine;
struct Doc(u32);

impl PdfEngine for Engine {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            name: "test",
            version: "0",
            capabilities: EngineCapabilities::default(),
        }
    }

    fn open(
        &self,
        source: DocumentSource,
        _: &OpenOptions,
    ) -> Result<Box<dyn EngineDocument>, EngineError> {
        Ok(Box::new(Doc(source.data.as_slice().len() as u32)))
    }
}

impl EngineDocument for Doc {
    fn page_count(&self) -> u32 {
        self.0
    }

    fn page_info(&self, _: PageIndex) -> Result<PageInfo, EngineError> {
        Ok(PageInfo {
            size: PageSize::LETTER,
            rotation: Rotation::R0,
        })
    }

    fn render(
        &self,
        _: &RenderRequest,
        _: &mut PixmapMut<'_>,
        _: &CancelToken,
    ) -> Result<RenderOutcome, EngineError> {
        Ok(RenderOutcome::default())
    }
}

const PATH: &str = r"C:\docs\book.pdf";
const IDENTITY: FileIdentity = FileIdentity {
    len: 40,
    modified: 7,
};

/// A document of `pages` pages opened from `PATH`.
fn opened(pages: usize) -> OpenedDocument {
    let loaded = LockedSource {
        path: PathBuf::from(PATH),
        source: DocumentSource::from_bytes(SharedBytes::from_vec(vec![b'%'; pages])),
        strategy: LoadStrategy::Read,
        file_bytes: pages,
        load_ms: 0.0,
        wrong_password: false,
        identity: Some(IDENTITY),
    };
    match open_loaded(&Engine, loaded, None) {
        Ok(doc) => doc,
        Err(e) => panic!("cannot open: {e}"),
    }
}

fn view(page: u32, zoom: SavedZoom, rotation: Rotation) -> SavedView {
    SavedView {
        page: PageIndex::new(page),
        offset: 0.25,
        x: 0.0,
        zoom,
        rotation,
    }
}

fn record(page: u32, page_count: u32) -> PositionRecord {
    PositionRecord {
        view: view(page, SavedZoom::Custom(2.5), Rotation::R90),
        page_count,
        identity: Some(IDENTITY),
    }
}

/// An empty store in its own scratch directory.
fn scratch_store(name: &str) -> (PositionStore, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "fastpdf-reading-start-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    (PositionStore::at(dir.join("positions.txt")), dir)
}

#[test]
fn without_a_store_nothing_is_looked_up() {
    let opened = with_start(opened(40), Some(IDENTITY), None);
    assert_eq!(opened.start, None);
    assert!(opened.session.is_none());
}

#[test]
fn records_not_written_yet_are_found_first() {
    let (store, dir) = scratch_store("pending");
    assert!(store.remember(Path::new(PATH), record(5, 40)));
    store.flush().unwrap();
    // Remembered again (closed at page 31) before the next write.
    assert!(store.remember(Path::new(PATH), record(30, 40)));
    let start = with_start(opened(40), Some(IDENTITY), Some(&store))
        .start
        .expect("asked");
    assert!(start.remembered);
    assert_eq!(start.identity, Some(IDENTITY));
    assert_eq!(
        start.restore.map(|r| r.view().page),
        Some(PageIndex::new(30))
    );
    assert_eq!(start.restore.map(|r| r.kind()), Some("exact"));
    // A changed file restores the place only.
    let other = FileIdentity {
        modified: 8,
        ..IDENTITY
    };
    let start = with_start(opened(40), Some(other), Some(&store)).start;
    assert_eq!(
        start.and_then(|s| s.restore).map(|r| r.kind()),
        Some("place")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_page_past_the_end_restores_nothing_but_counts_as_remembered() {
    let (store, dir) = scratch_store("past-the-end");
    assert!(store.remember(Path::new(PATH), record(30, 40)));
    let start = with_start(opened(3), Some(IDENTITY), Some(&store))
        .start
        .expect("asked");
    assert_eq!(start.restore, None);
    assert!(start.remembered, "closing refreshes the stale entry");
    // Never remembered at all.
    let (empty, empty_dir) = scratch_store("never");
    let start = with_start(opened(3), None, Some(&empty))
        .start
        .expect("asked");
    assert_eq!((start.restore, start.remembered), (None, false));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty_dir);
}

#[test]
fn an_open_looks_the_document_up_while_it_loads() {
    let (store, dir) = scratch_store("open");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("book.pdf");
    std::fs::write(&file, [b'%'; 12]).unwrap();
    let identity = FileIdentity::of(&file);
    let saved = PositionRecord {
        identity,
        ..record(8, 12)
    };
    assert!(store.remember(&file, saved));
    let opened = match open_for_reading(&Engine, &file, Some(&store), None) {
        Ok(opened) => opened,
        Err(e) => panic!("cannot open: {e}"),
    };
    let start = opened.start.expect("asked");
    assert!(start.remembered);
    assert_eq!(start.identity, identity);
    assert_eq!(start.restore, Some(Restore::Exact(saved.view)));
    // Positions off (no store given): nothing asked, the file not stat'ed.
    let plain = match open_for_reading(&Engine, &file, None, None) {
        Ok(opened) => opened,
        Err(e) => panic!("cannot open: {e}"),
    };
    assert_eq!(plain.start, None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A fresh session of `doc`, 800 x 1000 at scale 1.
fn session(doc: &OpenedDocument) -> DocumentSession<TileImage> {
    DocumentSession::new(
        Arc::clone(&doc.doc),
        SessionConfig::default(),
        (800.0, 1000.0),
        1.0,
        crate::textures::to_render_image,
        || {},
        |_| {},
    )
}

#[test]
fn an_exact_restore_keeps_its_own_zoom_and_rotation() {
    let doc = opened(40);
    let mut s = session(&doc);
    let saved = view(20, SavedZoom::Custom(2.5), Rotation::R90);
    // The default zoom (fit page) is not applied over it.
    apply_start(&mut s, Some(&Restore::Exact(saved)), DefaultZoom::FitPage);
    assert_eq!(s.zoom_mode(), ZoomMode::Custom);
    assert_eq!(s.zoom().percent(), 250);
    assert_eq!(s.rotation(), Rotation::R90);
    assert_eq!(s.saved_view().page, PageIndex::new(20));
}

#[test]
fn a_place_restore_comes_in_the_default_zoom() {
    let doc = opened(40);
    let mut s = session(&doc);
    let saved = view(20, SavedZoom::Custom(2.5), Rotation::R90);
    apply_start(&mut s, Some(&Restore::Place(saved)), DefaultZoom::FitPage);
    assert_eq!(s.zoom_mode(), ZoomMode::FitPage);
    assert_eq!(s.rotation(), Rotation::R0);
    assert_eq!(s.current_page(), PageIndex::new(20));
    // Actual size is a custom 100%, applied before the place.
    let mut s = session(&doc);
    apply_start(
        &mut s,
        Some(&Restore::Place(saved)),
        DefaultZoom::ActualSize,
    );
    assert_eq!((s.zoom_mode(), s.zoom().percent()), (ZoomMode::Custom, 100));
    assert_eq!(s.saved_view().page, PageIndex::new(20));
    // No start: the top of the document in the default zoom.
    let mut s = session(&doc);
    apply_start(&mut s, None, DefaultZoom::FitPage);
    assert_eq!(s.zoom_mode(), ZoomMode::FitPage);
    assert_eq!(s.current_page(), PageIndex::FIRST);
}
