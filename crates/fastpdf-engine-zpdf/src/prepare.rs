//! Page interpretation (serialized) and tile rasterization (parallel).
//!
//! A [`PreparedPage`] is zpdf's display list for one page plus the font and
//! image stores its commands refer to, all immutable and `Send + Sync`, so any
//! number of render threads can rasterize tiles from it at once.

use std::borrow::Cow;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use fastpdf_engine_api::{
    CancelToken, EngineError, PixelRect, PixelSize, PixmapMut, Rgba8, Rotation,
};
use zpdf_color::IccCache;
use zpdf_content::interpreter::ContentInterpreter;
use zpdf_core::{Matrix, ParseLimits, Rect};
use zpdf_display_list::{DisplayList, Path, PathElement, RenderCommand};
use zpdf_document::{OcConfig, OutputIntent, PdfDocument};
use zpdf_font::FontCache;
use zpdf_image::ImageCache;
use zpdf_render::{PageRenderInfo, RenderBackend};
use zpdf_render_cpu::CpuRenderer;

use crate::cache::Weighted;
use crate::convert::{
    self, PageGeometry, ZPDF_INTERPRET_BUDGET, ZPDF_MAX_COMMANDS, map_error, map_render_error,
};
use crate::fonts;
use crate::hairline::{HAIRLINE_GRID_PX, HairlinePolicy, Replacements};

/// Page-space (display-list space) bounding box of one paint command;
/// `None` means "unknown extent, never cull".
type Bounds = Option<Rect>;

/// One interpreted page, ready to be rasterized region by region.
pub(crate) struct PreparedPage {
    pub(crate) geometry: PageGeometry,
    dl: DisplayList,
    fonts: FontCache,
    images: ImageCache,
    /// Parallel to `dl.commands`.
    bounds: Vec<Bounds>,
    /// The interpreter hit one of zpdf's budgets and truncated the page.
    truncated: bool,
    /// Display list, images and bounds.
    base_weight: u64,
    /// Hairline replacements per render scale (`f32` bits), newest last.
    hairlines: Mutex<Vec<(u32, Arc<Replacements>)>>,
    hairline_bytes: AtomicU64,
}

/// Scales whose hairline replacements are kept per page.
const HAIRLINE_SCALES: usize = 2;

impl std::fmt::Debug for PreparedPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedPage")
            .field("commands", &self.dl.commands.len())
            .field("images", &self.images.len())
            .field("weight", &self.weight())
            .field("truncated", &self.truncated)
            .finish_non_exhaustive()
    }
}

impl Weighted for PreparedPage {
    fn weight(&self) -> u64 {
        self.base_weight
            .saturating_add(self.hairline_bytes.load(Ordering::Relaxed))
    }
}

/// Mutable per-document interpretation state; always used under the
/// document mutex because zpdf's `PdfDocument` is `!Sync`.
pub(crate) struct DocState {
    pub(crate) doc: PdfDocument,
    icc: IccCache,
    oc: Option<OcConfig>,
    intents: Vec<OutputIntent>,
}

impl DocState {
    pub(crate) fn new(doc: PdfDocument) -> Self {
        let oc = doc.oc_config();
        let intents = doc.output_intents();
        Self {
            doc,
            icc: IccCache::new(),
            oc,
            intents,
        }
    }

    pub(crate) fn geometry(&self, page: u32) -> Result<PageGeometry, EngineError> {
        let page = self.doc.page(page as usize).map_err(map_error)?;
        Ok(PageGeometry::new(page.effective_box(), page.rotate))
    }

    /// Interprets one page into a display list with the intrinsic `/Rotate`
    /// plus `user_rotation` baked in.
    pub(crate) fn prepare(
        &mut self,
        page_index: u32,
        user_rotation: Rotation,
        annotations: bool,
    ) -> Result<PreparedPage, EngineError> {
        let Self {
            doc,
            icc,
            oc,
            intents,
        } = self;
        let page = doc.page(page_index as usize).map_err(map_error)?;
        let geometry = PageGeometry::new(page.effective_box(), page.rotate);
        let total = geometry.rotation.then(user_rotation);
        let content = doc.page_content_bytes(&page).map_err(map_error)?;
        let mut fonts = fonts::page_fonts(doc, &page, &content);
        let mut annots = if annotations {
            doc.page_annotations(&page)
        } else {
            Vec::new()
        };
        if total != Rotation::R0 {
            // zpdf paints annotation appearances after resetting to the bare
            // rotation matrix, i.e. without the origin translation applied to
            // the page content below; shifting their rectangles instead keeps
            // them in place (without this they vanish off rotated pages whose
            // visible box does not start at the origin).
            let (dx, dy) = (geometry.visible.x0, geometry.visible.y0);
            for annot in &mut annots {
                let r = annot.rect;
                annot.rect = Rect::new(r.x0 - dx, r.y0 - dy, r.x1 - dx, r.y1 - dy);
            }
        }
        let cmyk_profile = zpdf_content::output_intent_cmyk_profile(
            doc.file(),
            doc.page_output_intents(&page),
            intents,
            icc,
        );
        let mut images = ImageCache::new();
        let (dl, stats) = {
            let mut interpreter = ContentInterpreter::new(geometry.visible)
                .with_page_rotation(total.degrees() as i32);
            if total != Rotation::R0 {
                // zpdf's with_page_rotation assumes a visible box at the origin;
                // with a non-zero CropBox/MediaBox origin the rotated content is
                // shifted off the page (verified in docs/audit/zpdf.md). Moving
                // the box origin to (0, 0) first restores it.
                interpreter = interpreter
                    .with_content_translation(-geometry.visible.x0, -geometry.visible.y0);
            }
            interpreter = interpreter
                .with_fonts(&mut fonts)
                .with_document(doc.file(), &page.resources)
                .with_images(&mut images)
                .with_colors(icc)
                .with_annotations(&annots);
            if let Some(oc) = oc.as_ref() {
                interpreter = interpreter.with_optional_content(oc);
            }
            if let Some(profile) = cmyk_profile {
                interpreter = interpreter.with_output_intent_cmyk(profile);
            }
            interpreter.interpret_with_stats(&content)
        };
        drop(content);
        let truncated = Duration::from_nanos(stats.total_ns) >= ZPDF_INTERPRET_BUDGET
            || dl.commands.len() >= ZPDF_MAX_COMMANDS;
        let bounds = command_bounds(&dl, &fonts);
        let weight = images
            .bytes_used()
            .saturating_add(display_list_bytes(&dl))
            .saturating_add((bounds.len() * std::mem::size_of::<Bounds>()) as u64);
        Ok(PreparedPage {
            geometry,
            dl,
            fonts,
            images,
            bounds,
            truncated,
            base_weight: weight,
            hairlines: Mutex::new(Vec::new()),
            hairline_bytes: AtomicU64::new(0),
        })
    }

    /// Text spans of one page in page space (unrotated, no images decoded).
    pub(crate) fn text(
        &mut self,
        page_index: u32,
    ) -> Result<(PageGeometry, Vec<zpdf_content::text::TextSpan>), EngineError> {
        let Self { doc, icc, oc, .. } = self;
        let page = doc.page(page_index as usize).map_err(map_error)?;
        let geometry = PageGeometry::new(page.effective_box(), page.rotate);
        let content = doc.page_content_bytes(&page).map_err(map_error)?;
        let mut fonts = fonts::page_fonts(doc, &page, &content);
        let mut spans = Vec::new();
        {
            // No image cache: zpdf then skips image and shading decoding.
            let mut interpreter = ContentInterpreter::new(geometry.visible)
                .with_fonts(&mut fonts)
                .with_document(doc.file(), &page.resources)
                .with_colors(icc)
                .with_text_sink(&mut spans);
            if let Some(oc) = oc.as_ref() {
                interpreter = interpreter.with_optional_content(oc);
            }
            let _ = interpreter.interpret(&content);
        }
        Ok((geometry, spans))
    }
}

/// Pixels rendered past every side of the requested region and discarded.
///
/// tiny-skia chops paths at the raster edge before rasterizing them, which
/// perturbs anti-aliased coverage in the pixels right next to that edge
/// (measured: a 3 px jog where a steep hairline entered a tile). Rendering a
/// small margin keeps those pixels out of the delivered region. Costs about
/// 3% more pixels on a 512 px tile.
pub(crate) const RASTER_MARGIN_PX: u32 = 4;

/// Inputs of one rasterization besides the prepared page.
pub(crate) struct RasterParams<'a> {
    pub(crate) region: PixelRect,
    pub(crate) scale: f32,
    pub(crate) background: Rgba8,
    pub(crate) limits: &'a ParseLimits,
    pub(crate) render_budget: Option<Duration>,
    /// Pixel size of the whole (rotated) page at `scale`.
    pub(crate) page_pixels: PixelSize,
}

impl PreparedPage {
    /// The font programs this page's text uses, as (identity, bytes). The
    /// identity is shared by every page holding the same program (zpdf hands
    /// pages the same font allocations), so callers can count each once.
    pub(crate) fn font_programs(&self) -> impl Iterator<Item = (usize, u64)> + '_ {
        // zpdf numbers a cache's fonts 0, 1, 2, ... and never evicts them.
        (0..self.fonts.len()).filter_map(|id| {
            let font = self.fonts.get(u32::try_from(id).ok()?)?;
            let identity = font.font_data.as_ref().map_or_else(
                || std::ptr::from_ref(font) as usize,
                |data| data.as_ptr() as usize,
            );
            Some((identity, font.estimated_cache_bytes()))
        })
    }

    /// The display-list-space rectangle of a raster whose top-left pixel is
    /// `(x, y)` (pixels of the rotated page at `scale`, may be negative) and
    /// whose size is `width` x `height` pixels.
    fn raster_rect(&self, x: f64, y: f64, width: u32, height: u32, scale: f32) -> Rect {
        let s = f64::from(scale);
        let page = self.dl.page_rect;
        let x0 = page.x0 + x / s;
        let y1 = page.y1 - y / s;
        // zpdf sizes the raster as ceil(width * scale); shaving a thousandth of
        // a pixel keeps that exactly equal to the requested size despite
        // floating-point error. Only the left/top edges position content.
        let w = (f64::from(width) - 1e-3) / s;
        let h = (f64::from(height) - 1e-3) / s;
        Rect::new(x0, y1 - h, x0 + w, y1)
    }

    /// The per-scale hairline replacements. They hold complete entries only,
    /// so a poisoned lock is still usable.
    fn hairline_entries(&self) -> MutexGuard<'_, Vec<(u32, Arc<Replacements>)>> {
        self.hairlines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Hairline replacements at `scale`, computed once per scale.
    fn hairlines(&self, scale: f32) -> Arc<Replacements> {
        let key = scale.to_bits();
        if let Some((_, found)) = self.hairline_entries().iter().find(|(k, _)| *k == key) {
            return Arc::clone(found);
        }
        // Computed outside the lock; a concurrent duplicate is harmless.
        let computed = Arc::new(Replacements::compute(&self.dl, scale));
        let mut entries = self.hairline_entries();
        if !entries.iter().any(|(k, _)| *k == key) {
            if entries.len() >= HAIRLINE_SCALES {
                entries.remove(0);
            }
            entries.push((key, Arc::clone(&computed)));
            let bytes = entries.iter().map(|(_, r)| r.bytes()).sum();
            self.hairline_bytes.store(bytes, Ordering::Relaxed);
        }
        computed
    }

    /// Rasterizes `params.region` into `target`.
    ///
    /// Paint commands whose bounds miss the region are skipped (structural
    /// clip/group commands always run, so the clip and group stacks stay
    /// balanced). The cancel token is polled before every command.
    pub(crate) fn rasterize(
        &self,
        params: &RasterParams<'_>,
        target: &mut PixmapMut<'_>,
        cancel: &CancelToken,
    ) -> Result<bool, EngineError> {
        let region = params.region;
        let margin = RASTER_MARGIN_PX;
        let size_error =
            || EngineError::LimitExceeded(fastpdf_engine_api::LimitKind::BitmapDimension);
        let raster_w = region
            .width
            .checked_add(2 * margin)
            .ok_or_else(size_error)?;
        let raster_h = region
            .height
            .checked_add(2 * margin)
            .ok_or_else(size_error)?;
        let rect = self.raster_rect(
            f64::from(region.x) - f64::from(margin),
            f64::from(region.y) - f64::from(margin),
            raster_w,
            raster_h,
            params.scale,
        );
        let mut renderer = CpuRenderer::new()
            .with_limits(params.limits)
            .with_fonts(&self.fonts)
            .with_images(&self.images)
            .with_render_budget(params.render_budget);
        let started = Instant::now();
        renderer
            .begin_page(&PageRenderInfo {
                page_rect: rect,
                scale: params.scale,
                background: convert::color(params.background),
            })
            .map_err(map_render_error)?;
        // Anti-aliasing and hairline strokes reach up to about a pixel past the
        // geometry; two pixels of slack keeps culling exact.
        let slack = 2.0 / f64::from(params.scale);
        let (cx0, cy0, cx1, cy1) = (
            rect.x0 - slack,
            rect.y0 - slack,
            rect.x1 + slack,
            rect.y1 + slack,
        );
        // Grid-aligned regions (all scheduler tiles) share one cached set of
        // hairline replacements per scale; anything else decides per raster.
        let cached = grid_aligned(region, params.page_pixels).then(|| self.hairlines(params.scale));
        let per_raster = cached
            .is_none()
            .then(|| HairlinePolicy::for_raster(&rect, &self.dl.page_rect, params.scale));
        for (index, (command, bounds)) in self.dl.commands.iter().zip(&self.bounds).enumerate() {
            if cancel.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            if let Some(b) = bounds
                && (b.x1 < cx0 || b.x0 > cx1 || b.y1 < cy0 || b.y0 > cy1)
            {
                continue;
            }
            let replacement: Option<Cow<'_, [RenderCommand]>> = match (&cached, &per_raster) {
                (Some(cached), _) => cached.get(index).map(Cow::Borrowed),
                (None, Some(policy)) => policy.replace(command).map(Cow::Owned),
                (None, None) => None,
            };
            match replacement {
                None => renderer.execute(command).map_err(map_render_error)?,
                Some(commands) => {
                    for replacement in commands.iter() {
                        renderer.execute(replacement).map_err(map_render_error)?;
                    }
                }
            }
        }
        let page = renderer.end_page().map_err(map_render_error)?;
        if page.width != raster_w || page.height != raster_h {
            return Err(EngineError::Internal(format!(
                "zpdf produced {}x{} pixels for a {raster_w}x{raster_h} raster",
                page.width, page.height
            )));
        }
        // Deliver the region without its margin.
        let stride = raster_w as usize * 4;
        let offset = margin as usize * stride + margin as usize * 4;
        let pixels = page
            .data
            .get(offset..)
            .ok_or_else(|| EngineError::Internal("raster smaller than its margin".into()))?;
        target.copy_from_rgba(pixels, stride)?;
        let over_time = params
            .render_budget
            .is_some_and(|budget| started.elapsed() >= budget);
        Ok(self.truncated || over_time)
    }
}

/// True when every border of `region` lies on the hairline grid or on the
/// page edge.
fn grid_aligned(region: PixelRect, page: PixelSize) -> bool {
    let grid = u64::from(HAIRLINE_GRID_PX);
    let on_grid = |v: u64, edge: u32| v.is_multiple_of(grid) || v == u64::from(edge);
    on_grid(u64::from(region.x), page.width)
        && on_grid(u64::from(region.y), page.height)
        && on_grid(region.right(), page.width)
        && on_grid(region.bottom(), page.height)
}

/// Rough retained size of a display list.
fn display_list_bytes(dl: &DisplayList) -> u64 {
    let command = std::mem::size_of::<RenderCommand>() as u64;
    let element = std::mem::size_of::<PathElement>() as u64;
    let glyph = std::mem::size_of::<zpdf_display_list::PositionedGlyph>() as u64;
    dl.commands.iter().fold(0u64, |sum, cmd| {
        let extra = match cmd {
            RenderCommand::FillPath { path, .. }
            | RenderCommand::StrokePath { path, .. }
            | RenderCommand::PushClip { path, .. }
            | RenderCommand::PushClipStroke { path, .. } => path.elements.len() as u64 * element,
            RenderCommand::DrawGlyphRun(run) => run.glyphs.len() as u64 * glyph,
            _ => 0,
        };
        sum.saturating_add(command).saturating_add(extra)
    })
}

/// Conservative page-space bounds for every paint command.
fn command_bounds(dl: &DisplayList, fonts: &FontCache) -> Vec<Bounds> {
    dl.commands
        .iter()
        .map(|command| match command {
            RenderCommand::FillPath { path, .. } => path_bounds(path, 0.0),
            RenderCommand::StrokePath { path, style, .. } => {
                // Miter joins reach miter_limit/2 * width past the centerline.
                let reach =
                    f64::from(style.width.abs()) * f64::from(style.miter_limit.max(2.0)) * 0.5;
                path_bounds(path, reach)
            }
            RenderCommand::DrawGlyphRun(run) => glyph_run_bounds(run, fonts),
            RenderCommand::DrawImage(image) => unit_square_bounds(&image.transform),
            // Clips and groups change state; they are never skipped.
            _ => None,
        })
        .collect()
}

fn finite_bounds(b: Rect) -> Bounds {
    [b.x0, b.y0, b.x1, b.y1]
        .iter()
        .all(|v| v.is_finite())
        .then_some(b)
}

fn path_bounds(path: &Path, pad: f64) -> Bounds {
    let mut b = Accumulator::new();
    for element in &path.elements {
        match *element {
            PathElement::MoveTo(p) | PathElement::LineTo(p) => b.add(p.x, p.y),
            // A cubic Bézier lies inside the hull of its control points.
            PathElement::CurveTo(c1, c2, p) => {
                b.add(c1.x, c1.y);
                b.add(c2.x, c2.y);
                b.add(p.x, p.y);
            }
            PathElement::Close => {}
        }
    }
    b.finish(pad)
}

/// Glyphs rarely reach beyond this many ems from their origin; the box is
/// generous on purpose because a too-small box would drop ink at tile edges.
const GLYPH_REACH_EM: f64 = 4.0;

fn glyph_run_bounds(run: &zpdf_display_list::GlyphRun, fonts: &FontCache) -> Bounds {
    // Type3 glyphs are arbitrary content streams: their extent is unknown.
    let font = fonts.get(run.font_id)?;
    if font.is_type3() {
        return None;
    }
    let size = f64::from(run.font_size).abs();
    let reach_x = size * f64::from(run.h_scale).abs().max(1.0) * GLYPH_REACH_EM;
    let reach_y = size * GLYPH_REACH_EM;
    let mut b = Accumulator::new();
    for glyph in &run.glyphs {
        let (gx, gy) = (f64::from(glyph.x), f64::from(glyph.y));
        for (dx, dy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
            let (x, y) = apply(&run.transform, gx + dx * reach_x, gy + dy * reach_y);
            b.add(x, y);
        }
    }
    b.finish(0.0)
}

fn unit_square_bounds(m: &Matrix) -> Bounds {
    let mut b = Accumulator::new();
    for (x, y) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
        let (px, py) = apply(m, x, y);
        b.add(px, py);
    }
    b.finish(0.0)
}

fn apply(m: &Matrix, x: f64, y: f64) -> (f64, f64) {
    (m.a * x + m.c * y + m.e, m.b * x + m.d * y + m.f)
}

struct Accumulator {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl Accumulator {
    fn new() -> Self {
        Self {
            x0: f64::INFINITY,
            y0: f64::INFINITY,
            x1: f64::NEG_INFINITY,
            y1: f64::NEG_INFINITY,
        }
    }

    fn add(&mut self, x: f64, y: f64) {
        self.x0 = self.x0.min(x);
        self.y0 = self.y0.min(y);
        self.x1 = self.x1.max(x);
        self.y1 = self.y1.max(y);
    }

    /// `None` for an empty or non-finite accumulation (then never culled).
    fn finish(self, pad: f64) -> Bounds {
        if self.x0 > self.x1 || self.y0 > self.y1 || !pad.is_finite() {
            return None;
        }
        finite_bounds(Rect::new(
            self.x0 - pad,
            self.y0 - pad,
            self.x1 + pad,
            self.y1 + pad,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zpdf_core::Point;

    #[test]
    fn path_bounds_cover_control_points_and_padding() {
        let mut path = Path::new();
        path.move_to(Point::new(10.0, 20.0));
        path.curve_to(
            Point::new(0.0, 50.0),
            Point::new(40.0, 60.0),
            Point::new(30.0, 25.0),
        );
        let b = path_bounds(&path, 1.0).unwrap();
        assert_eq!((b.x0, b.y0, b.x1, b.y1), (-1.0, 19.0, 41.0, 61.0));
        assert!(path_bounds(&Path::new(), 1.0).is_none());
        let mut nan = Path::new();
        nan.move_to(Point::new(f64::NAN, 0.0));
        assert!(path_bounds(&nan, 0.0).is_none());
    }

    #[test]
    fn image_bounds_follow_the_transform() {
        let m = Matrix::new(100.0, 0.0, 0.0, -50.0, 10.0, 70.0);
        let b = unit_square_bounds(&m).unwrap();
        assert_eq!((b.x0, b.y0, b.x1, b.y1), (10.0, 20.0, 110.0, 70.0));
    }
}
