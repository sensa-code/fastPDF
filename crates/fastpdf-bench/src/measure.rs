//! In-process measurements for `open`, `render` and `full`.

use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use fastpdf_core::loader;
use fastpdf_engine_api::{
    CancelToken, ColorMode, DocumentId, DocumentSource, EngineDocument, EngineError,
    GuardedDocument, OpenOptions, PageId, PageIndex, PdfEngine, PixelFormat, PixelRect, Pixmap,
    RenderRequest, RenderScale, Rgba8, Rotation, open_guarded,
};
use fastpdf_render::{
    PlannedTile, Priority, RenderScheduler, ScaleBucket, SchedulerConfig, TileGrid, TileKey,
};

use crate::args::Args;
use crate::png;
use crate::report::{FileReport, PageError, Stats, Status, ms};

/// Sidebar thumbnail width in device pixels.
const THUMBNAIL_WIDTH_PX: f32 = 160.0;

struct Opened {
    doc: Arc<GuardedDocument>,
    read: Duration,
    open: Duration,
}

fn open_document(
    engine: &dyn PdfEngine,
    file: &Path,
    args: &Args,
    report: &mut FileReport,
) -> Option<Opened> {
    let started = Instant::now();
    let loaded = match loader::load(file) {
        Ok(l) => l,
        Err(e) => {
            report.status = Status::LoadError;
            report.error = Some(e.to_string());
            return None;
        }
    };
    let read = started.elapsed();
    report.file_bytes = Some(loaded.bytes.len() as u64);
    report.load_strategy = Some(loaded.strategy.to_string());

    let options = OpenOptions {
        password: args.password.clone(),
        ..OpenOptions::default()
    };
    let started = Instant::now();
    let source = DocumentSource::from_bytes(loaded.bytes).with_path(file);
    match open_guarded(engine, source, &options) {
        Ok(doc) => {
            let open = started.elapsed();
            report.page_count = Some(doc.page_count());
            Some(Opened {
                doc: Arc::new(doc),
                read,
                open,
            })
        }
        Err(e) => {
            report.status = Status::OpenError;
            report.error = Some(e.to_string());
            report.read_ms = Some(ms(read));
            report.open_ms = Some(ms(started.elapsed()));
            None
        }
    }
}

/// `open`: read + engine open + first page geometry, repeated.
pub(crate) fn open(engine: &dyn PdfEngine, file: &Path, args: &Args) -> FileReport {
    let mut report = FileReport::new("open", file, engine, args);
    let mut opens = Vec::new();
    let mut reads = Vec::new();
    let mut metas = Vec::new();
    for _ in 0..args.repeat {
        let Some(opened) = open_document(engine, file, args, &mut report) else {
            break;
        };
        let started = Instant::now();
        let meta = opened
            .doc
            .page_info(PageIndex::FIRST)
            .and_then(|_| opened.doc.metadata());
        metas.push(ms(started.elapsed()));
        if let Err(e) = meta {
            push_error(&mut report, PageIndex::FIRST, "metadata", &e);
        }
        reads.push(ms(opened.read));
        opens.push(ms(opened.open));
    }
    report.read_ms = Stats::from_samples(&reads).map(|s| s.median);
    report.open_ms = Stats::from_samples(&opens)
        .map(|s| s.median)
        .or(report.open_ms);
    report.metadata_ms = Stats::from_samples(&metas).map(|s| s.median);
    report.samples_ms = Stats::from_samples(&opens);
    finish(&mut report);
    report
}

/// `render`: one page, full bitmap or tiles through the scheduler, repeated.
pub(crate) fn render(engine: &dyn PdfEngine, file: &Path, args: &Args) -> FileReport {
    let mut report = FileReport::new("render", file, engine, args);
    let Some(opened) = open_document(engine, file, args, &mut report) else {
        finish(&mut report);
        return report;
    };
    report.read_ms = Some(ms(opened.read));
    report.open_ms = Some(ms(opened.open));
    let page = PageIndex::new(args.page - 1);
    let bucket = ScaleBucket::for_display_scale(args.scale);
    let mut samples = Vec::new();
    for i in 0..args.repeat {
        let started = Instant::now();
        let result = match args.tile {
            None => render_full(&opened.doc, page, bucket.render_scale()).map(|pm| (pm, 1)),
            Some(tile) => render_tiled(&opened.doc, page, bucket, tile, args),
        };
        let elapsed = ms(started.elapsed());
        match result {
            Ok((pixmap, tiles)) => {
                samples.push(elapsed);
                report.tiles = Some(tiles);
                if i == 0 {
                    report.first_page_ms = Some(elapsed);
                    if let Some(out) = &args.out
                        && let Err(e) = png::write_rgba(out, &pixmap)
                    {
                        report.error = Some(format!("cannot write {}: {e}", out.display()));
                    }
                }
            }
            Err(e) => {
                push_error(&mut report, page, "render", &e);
                break;
            }
        }
    }
    report.samples_ms = Stats::from_samples(&samples);
    finish(&mut report);
    report
}

/// `full`: the spec §26 metric set for one file.
pub(crate) fn full(engine: &dyn PdfEngine, file: &Path, args: &Args) -> FileReport {
    let mut report = FileReport::new("full", file, engine, args);
    let Some(opened) = open_document(engine, file, args, &mut report) else {
        finish(&mut report);
        return report;
    };
    let doc = &opened.doc;
    report.read_ms = Some(ms(opened.read));
    report.open_ms = Some(ms(opened.open));
    let bucket = ScaleBucket::for_display_scale(args.scale);

    let started = Instant::now();
    let meta = doc.page_info(PageIndex::FIRST).and_then(|_| doc.metadata());
    let metadata = started.elapsed();
    report.metadata_ms = Some(ms(metadata));
    if let Err(e) = meta {
        push_error(&mut report, PageIndex::FIRST, "metadata", &e);
    }

    let started = Instant::now();
    let first = match args.tile {
        None => render_full(doc, PageIndex::FIRST, bucket.render_scale()).map(|_| 1),
        Some(tile) => render_tiled(doc, PageIndex::FIRST, bucket, tile, args).map(|(_, n)| n),
    };
    let first_elapsed = started.elapsed();
    match first {
        Ok(tiles) => {
            report.first_page_ms = Some(ms(first_elapsed));
            report.time_to_first_page_ms =
                Some(ms(opened.read + opened.open + metadata + first_elapsed));
            report.tiles = Some(tiles);
        }
        Err(e) => push_error(&mut report, PageIndex::FIRST, "first_page", &e),
    }

    let page_count = doc.page_count();
    let mut renders = Vec::new();
    for page in sample_pages(page_count, args.samples) {
        let started = Instant::now();
        match render_full(doc, page, bucket.render_scale()) {
            Ok(_) => renders.push(ms(started.elapsed())),
            Err(e) => push_error(&mut report, page, "page_render", &e),
        }
    }
    report.page_render_ms = Stats::from_samples(&renders);

    let started = Instant::now();
    match doc.text_layer(PageIndex::FIRST, &CancelToken::new()) {
        Ok(layer) => {
            report.text_ms = Some(ms(started.elapsed()));
            report.text_chars = Some(layer.spans.iter().map(|s| s.text.chars().count()).sum());
        }
        Err(EngineError::Unsupported(_)) => {}
        Err(e) => push_error(&mut report, PageIndex::FIRST, "text", &e),
    }

    let mut thumbs = Vec::new();
    for page in (0..page_count.min(args.samples as u32)).map(PageIndex::new) {
        let started = Instant::now();
        let scale = doc.page_info(page).and_then(|info| {
            let width = info.display_size(Rotation::R0).width.max(1.0);
            RenderScale::new(THUMBNAIL_WIDTH_PX / width)
                .ok_or_else(|| EngineError::InvalidRequest("thumbnail scale out of range".into()))
        });
        match scale.and_then(|s| render_full(doc, page, s)) {
            Ok(_) => thumbs.push(ms(started.elapsed())),
            Err(e) => push_error(&mut report, page, "thumbnail", &e),
        }
    }
    report.thumbnail_ms = Stats::from_samples(&thumbs);

    finish(&mut report);
    report
}

pub(crate) fn render_full(
    doc: &GuardedDocument,
    page: PageIndex,
    scale: RenderScale,
) -> Result<Pixmap, EngineError> {
    let info = doc.page_info(page)?;
    let request = RenderRequest::full_page(page, info.size, info.rotation, Rotation::R0, scale);
    let mut pixmap = Pixmap::new(request.region.size(), PixelFormat::default(), doc.limits())?;
    doc.render(&request, &mut pixmap.as_mut(), &CancelToken::new())?;
    Ok(pixmap)
}

/// Renders the tiles of `page` visible in `--viewport` (default: the whole
/// page) through the real scheduler, and assembles them into one bitmap.
fn render_tiled(
    doc: &Arc<GuardedDocument>,
    page: PageIndex,
    bucket: ScaleBucket,
    tile: u32,
    args: &Args,
) -> Result<(Pixmap, u64), EngineError> {
    let info = doc.page_info(page)?;
    let scale = bucket.render_scale();
    let page_px = scale.page_pixels(info.size, info.rotation);
    let grid = TileGrid::new(page_px, tile);
    let area = match args.viewport {
        Some((w, h)) => PixelRect::new(0, 0, w, h)
            .intersect(page_px.bounds())
            .unwrap_or(page_px.bounds()),
        None => page_px.bounds(),
    };
    let plan: Vec<PlannedTile> = grid
        .tiles_intersecting(area)
        .filter_map(|coord| {
            let region = grid.tile_rect(coord)?;
            Some(PlannedTile {
                key: TileKey {
                    page: PageId::new(DocumentId::from_raw(1), page),
                    bucket,
                    rotation: Rotation::R0,
                    color: ColorMode::Normal,
                    tile_size: grid.tile_size(),
                    coord,
                },
                priority: Priority::Visible,
                distance: region.y as f32,
                request: RenderRequest {
                    page,
                    scale,
                    rotation: Rotation::R0,
                    region,
                    background: Rgba8::WHITE,
                    color_mode: ColorMode::Normal,
                    annotations: true,
                },
            })
        })
        .collect();
    let expected = plan.len();

    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let config = SchedulerConfig {
        workers: args
            .workers
            .unwrap_or_else(SchedulerConfig::default_workers),
        ..SchedulerConfig::default()
    };
    let document: Arc<dyn EngineDocument> = doc.clone();
    let scheduler = RenderScheduler::new(document, config, move |result| {
        if let Ok(tx) = tx.lock() {
            let _ = tx.send(result);
        }
    });
    scheduler.submit_plan(plan);

    let mut canvas = Pixmap::new(area.size(), PixelFormat::default(), doc.limits())?;
    let mut first_error = None;
    for _ in 0..expected {
        let result = rx
            .recv_timeout(Duration::from_secs(600))
            .map_err(|_| EngineError::Internal("tile render timed out".into()))?;
        match result.result {
            Ok(tile_px) => {
                let region = grid.tile_rect(result.key.coord).unwrap_or_default();
                blit(&mut canvas, area, &tile_px, region);
            }
            Err(e) => first_error = first_error.or(Some(e)),
        }
    }
    match first_error {
        Some(e) => Err(e),
        None => Ok((canvas, expected as u64)),
    }
}

/// Copies the part of `tile` (at `region` in page pixels) that overlaps
/// `area` into `canvas` (which covers `area`).
fn blit(canvas: &mut Pixmap, area: PixelRect, tile: &Pixmap, region: PixelRect) {
    let Some(overlap) = region.intersect(area) else {
        return;
    };
    let cw = canvas.size().width as usize;
    let tw = tile.size().width as usize;
    let row_bytes = overlap.width as usize * 4;
    let src = tile.data();
    let mut view = canvas.as_mut();
    let dst = view.data_mut();
    for y in 0..overlap.height as usize {
        let sy = (overlap.y - region.y) as usize + y;
        let sx = (overlap.x - region.x) as usize;
        let dy = (overlap.y - area.y) as usize + y;
        let dx = (overlap.x - area.x) as usize;
        let s = (sy * tw + sx) * 4;
        let d = (dy * cw + dx) * 4;
        dst[d..d + row_bytes].copy_from_slice(&src[s..s + row_bytes]);
    }
}

/// Up to `n` pages spread evenly over the document, excluding page 1
/// (measured separately as the first page).
fn sample_pages(page_count: u32, n: usize) -> Vec<PageIndex> {
    let rest = page_count.saturating_sub(1) as usize;
    let n = n.min(rest);
    (0..n)
        .map(|i| PageIndex::new(1 + (i * rest / n.max(1)) as u32))
        .collect()
}

fn push_error(report: &mut FileReport, page: PageIndex, stage: &str, e: &EngineError) {
    report.page_errors.push(PageError {
        page: page.display_number(),
        stage: stage.to_owned(),
        error: e.to_string(),
    });
}

fn finish(report: &mut FileReport) {
    if report.status == Status::Ok && !report.page_errors.is_empty() {
        report.status = Status::Partial;
    }
    report.capture_process_metrics();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_are_spread_and_skip_page_one() {
        let pages: Vec<u32> = sample_pages(2000, 4).iter().map(|p| p.get()).collect();
        assert_eq!(pages, vec![1, 500, 1000, 1500]);
        assert!(sample_pages(1, 10).is_empty());
        assert_eq!(sample_pages(3, 10).len(), 2);
    }

    #[test]
    fn blit_copies_overlap_only() {
        let limits = fastpdf_engine_api::ResourceLimits::default();
        let area = PixelRect::new(0, 0, 4, 2);
        let mut canvas = Pixmap::new(area.size(), PixelFormat::default(), &limits).unwrap();
        let mut tile = Pixmap::new(
            fastpdf_engine_api::PixelSize::new(2, 2),
            PixelFormat::default(),
            &limits,
        )
        .unwrap();
        tile.as_mut().fill(Rgba8::WHITE);
        blit(&mut canvas, area, &tile, PixelRect::new(3, 1, 2, 2));
        let px = |x: usize, y: usize| canvas.data()[(y * 4 + x) * 4];
        assert_eq!(px(3, 1), 255);
        assert_eq!(px(2, 1), 0);
        assert_eq!(px(3, 0), 0);
    }
}
