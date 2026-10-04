//! GPU texture lifetime for tiles and thumbnails (docs/audit/gpui.md,
//! "Texture & Image Pipeline").
//!
//! GPUI uploads an `Arc<RenderImage>` into its sprite atlas the first time it
//! is painted and keeps it there until `Window::drop_image` is called —
//! dropping the `Arc` alone leaks the atlas space for the lifetime of the
//! window. This module makes that lifetime explicit:
//!
//! * Images evicted by the tile or thumbnail caches (budget, memory
//!   pressure, document close) are pushed into a [`RetireQueue`] from
//!   whatever thread evicts them; the UI thread releases them with
//!   `drop_image` at the start of the next frame, before anything new is
//!   uploaded.
//! * Every image painted at least once is tracked as *resident*, so closing
//!   a document can release all of them even if an eviction were missed.
//! * Uploads are budgeted per frame: [`TileTextures::reserve`] is the
//!   `ready` predicate of `DocumentSession::frame_with`, so an image over the
//!   budget is replaced by its stand-in for this frame and uploaded later.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use fastpdf_engine_api::{PixelFormat, Pixmap};
use gpui::{Bounds, Corners, ImageId, Pixels, RenderImage, Window, point, px, size};
use image::{Frame, RgbaImage};

/// What a cached tile or thumbnail is in the UI: an image GPUI can paint.
pub(crate) type TileImage = Arc<RenderImage>;

/// Default per-frame upload budget. At ~0.06 ms per MiB (measured on GPUI
/// main, docs/audit/gpui.md) 32 MiB is about 2 ms — a full 1080p screen of
/// 512 px tiles fits in one frame, a 4K screen in two or three.
pub const DEFAULT_UPLOAD_BUDGET: usize = 32 * 1024 * 1024;

/// Converts a rendered tile into a GPUI image. Runs on render workers.
///
/// GPUI wants BGRA with straight alpha. Tiles are rendered onto opaque
/// paper, so premultiplied and straight alpha are the same bytes and the
/// buffer is moved without copying; translucent pixels (a transparent paper
/// color) are un-premultiplied.
pub(crate) fn to_render_image(pixmap: Pixmap) -> TileImage {
    let size = pixmap.size();
    let format = pixmap.format();
    let mut data = pixmap.into_data();
    let (pixels, _) = data.as_chunks_mut::<4>();
    for px in pixels.iter_mut() {
        if format == PixelFormat::Rgba8Premultiplied {
            px.swap(0, 2);
        }
        let a = px[3];
        if a != 255 && a != 0 {
            for c in &mut px[..3] {
                *c = ((u16::from(*c) * 255 + u16::from(a) / 2) / u16::from(a)).min(255) as u8;
            }
        }
    }
    let buffer = RgbaImage::from_raw(size.width, size.height, data).unwrap_or_else(|| {
        // Unreachable for a well-formed Pixmap (len == w * h * 4); never
        // panic on the render path, draw nothing instead.
        log::error!("tile buffer does not match {}x{}", size.width, size.height);
        RgbaImage::new(1, 1)
    });
    Arc::new(RenderImage::new([Frame::new(buffer)]))
}

fn image_bytes(image: &RenderImage) -> usize {
    image.as_bytes(0).map_or(0, <[u8]>::len)
}

/// Images waiting for `drop_image` on the UI thread.
#[derive(Clone, Default)]
pub(crate) struct RetireQueue {
    inner: Arc<RetireInner>,
}

#[derive(Default)]
struct RetireInner {
    images: Mutex<Vec<TileImage>>,
    /// Set while the UI thread is inside a session call made during a frame
    /// (`frame_with`, `thumbnails`); the frame itself releases evictions
    /// made there, so they need no extra wake-up.
    in_frame: AtomicBool,
}

impl std::fmt::Debug for RetireQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetireQueue")
            .field("len", &self.lock().len())
            .finish()
    }
}

impl RetireQueue {
    fn lock(&self) -> MutexGuard<'_, Vec<TileImage>> {
        self.inner.images.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queues evicted images. Returns true when the UI must be woken to
    /// release them (i.e. the eviction did not happen inside a frame).
    pub(crate) fn retire(&self, images: impl IntoIterator<Item = TileImage>) -> bool {
        self.lock().extend(images);
        !self.inner.in_frame.load(Ordering::Acquire)
    }

    /// Runs `f` with evictions marked as happening inside the current frame.
    pub(crate) fn in_frame<R>(&self, f: impl FnOnce() -> R) -> R {
        self.inner.in_frame.store(true, Ordering::Release);
        let result = f();
        self.inner.in_frame.store(false, Ordering::Release);
        result
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn take(&self) -> Vec<TileImage> {
        std::mem::take(&mut *self.lock())
    }
}

/// Counters for the development overlay.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TextureStats {
    pub resident: usize,
    pub resident_bytes: usize,
    pub uploads: u64,
    pub upload_bytes: u64,
    pub released: u64,
    /// Draws postponed by the per-frame upload budget.
    pub deferred: u64,
    pub frame_upload_bytes: usize,
}

/// Tracks which images live in GPUI's sprite atlas.
pub(crate) struct TileTextures {
    retire: RetireQueue,
    resident: HashMap<ImageId, TileImage>,
    resident_bytes: usize,
    upload_budget: usize,
    /// Frame the budget below belongs to.
    frame: Option<u64>,
    frame_uploaded: usize,
    /// Images allowed to upload this frame.
    reserved: HashSet<ImageId>,
    stats: TextureStats,
}

impl std::fmt::Debug for TileTextures {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TileTextures")
            .field("resident", &self.resident.len())
            .field("resident_bytes", &self.resident_bytes)
            .finish_non_exhaustive()
    }
}

impl TileTextures {
    #[cfg(test)]
    pub(crate) fn new(upload_budget: usize) -> Self {
        Self::with_retire_queue(upload_budget, RetireQueue::default())
    }

    /// Uses `retire`, a queue sessions created before this window (startup)
    /// already evict into.
    pub(crate) fn with_retire_queue(upload_budget: usize, retire: RetireQueue) -> Self {
        Self {
            retire,
            resident: HashMap::new(),
            resident_bytes: 0,
            upload_budget: upload_budget.max(1),
            frame: None,
            frame_uploaded: 0,
            reserved: HashSet::new(),
            stats: TextureStats::default(),
        }
    }

    pub(crate) fn retire_queue(&self) -> RetireQueue {
        self.retire.clone()
    }

    pub(crate) fn stats(&self) -> TextureStats {
        TextureStats {
            resident: self.resident.len(),
            resident_bytes: self.resident_bytes,
            frame_upload_bytes: self.frame_uploaded,
            ..self.stats
        }
    }

    /// Starts frame `frame`: resets the upload budget and releases retired
    /// images. Idempotent per frame, so every element that paints images
    /// calls it first in its prepaint.
    pub(crate) fn begin_frame(&mut self, frame: u64, window: &mut Window) {
        if self.start_frame(frame) {
            for image in self.retire.take() {
                self.release(image, window);
            }
        }
    }

    /// Budget bookkeeping of [`Self::begin_frame`]; true for a new frame.
    fn start_frame(&mut self, frame: u64) -> bool {
        if self.frame == Some(frame) {
            return false;
        }
        self.frame = Some(frame);
        self.frame_uploaded = 0;
        self.reserved.clear();
        true
    }

    /// Whether evicted images are still waiting for release.
    pub(crate) fn has_retired(&self) -> bool {
        !self.retire.is_empty()
    }

    fn release(&mut self, image: TileImage, window: &mut Window) {
        self.forget(&image);
        // Harmless for images that never reached the atlas.
        if let Err(e) = window.drop_image(image) {
            log::warn!("drop_image failed: {e}");
        }
    }

    /// Bookkeeping half of [`Self::release`].
    fn forget(&mut self, image: &TileImage) {
        if let Some(resident) = self.resident.remove(&image.id) {
            self.resident_bytes = self.resident_bytes.saturating_sub(image_bytes(&resident));
            self.stats.released += 1;
        }
    }

    /// The `ready` predicate for `DocumentSession::frame_with`: whether
    /// `image` may be drawn this frame. Images already in the atlas always
    /// may; new ones only while this frame's upload budget lasts (the first
    /// upload of a frame always proceeds, so progress is guaranteed).
    pub(crate) fn reserve(&mut self, image: &TileImage) -> bool {
        let id = image.id;
        if self.resident.contains_key(&id) || self.reserved.contains(&id) {
            return true;
        }
        let bytes = image_bytes(image);
        if self.frame_uploaded > 0 && self.frame_uploaded + bytes > self.upload_budget {
            self.stats.deferred += 1;
            return false;
        }
        self.frame_uploaded += bytes;
        self.reserved.insert(id);
        true
    }

    /// Bookkeeping half of [`Self::paint`]: records the upload of `image`
    /// if this is its first paint.
    fn note_paint(&mut self, image: &TileImage) {
        if self.resident.contains_key(&image.id) {
            return;
        }
        let bytes = image_bytes(image);
        if !self.reserved.contains(&image.id) {
            // Not budgeted through `reserve` (small images such as
            // thumbnails): still count it against this frame.
            self.frame_uploaded += bytes;
        }
        self.resident_bytes += bytes;
        self.resident.insert(image.id, Arc::clone(image));
        self.stats.uploads += 1;
        self.stats.upload_bytes += bytes as u64;
    }

    /// Paints `image` into `dest`; `src` is the part of the image to show,
    /// `[x, y, width, height]` in image pixels (`None`: all of it). Uploads
    /// the image on its first paint.
    pub(crate) fn paint(
        &mut self,
        image: &TileImage,
        dest: Bounds<Pixels>,
        src: Option<[f32; 4]>,
        window: &mut Window,
    ) {
        self.note_paint(image);
        let full = image.size(0);
        let image_size = (u32::from(full.width) as f32, u32::from(full.height) as f32);
        let image_bounds = image_bounds_for(dest, src, image_size);
        if let Err(e) = window.paint_image(
            dest,
            image_bounds,
            Corners::default(),
            Arc::clone(image),
            0,
            false,
        ) {
            log::warn!("paint_image failed: {e}");
        }
    }

    /// Releases every image this window uploaded (document closed).
    pub(crate) fn release_all(&mut self, window: &mut Window) {
        for image in self.retire.take() {
            self.release(image, window);
        }
        let resident: Vec<TileImage> = self.resident.values().cloned().collect();
        for image in resident {
            self.release(image, window);
        }
        debug_assert!(self.resident.is_empty());
        self.resident_bytes = 0;
    }
}

/// Where the whole image must be placed so that its `src` part lands
/// exactly on `dest`. GPUI's `paint_image` then draws `dest ∩ image_bounds`
/// (= `dest`) and samples only inside `src`, so tile gutters keep bilinear
/// filtering at the edges from reading neighboring atlas entries.
fn image_bounds_for(
    dest: Bounds<Pixels>,
    src: Option<[f32; 4]>,
    (image_w, image_h): (f32, f32),
) -> Bounds<Pixels> {
    let Some([sx, sy, sw, sh]) = src else {
        return dest;
    };
    if sw <= 0.0 || sh <= 0.0 {
        return dest;
    }
    let scale_x = f32::from(dest.size.width) / sw;
    let scale_y = f32::from(dest.size.height) / sh;
    Bounds::new(
        point(
            dest.origin.x - px(sx * scale_x),
            dest.origin.y - px(sy * scale_y),
        ),
        size(px(image_w * scale_x), px(image_h * scale_y)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastpdf_engine_api::{PixelSize, ResourceLimits};

    fn pixmap(format: PixelFormat, px: [u8; 4]) -> Pixmap {
        sized_pixmap(format, px, 2, 1)
    }

    fn sized_pixmap(format: PixelFormat, px: [u8; 4], w: u32, h: u32) -> Pixmap {
        let mut p = Pixmap::new(PixelSize::new(w, h), format, &ResourceLimits::default())
            .expect("tiny pixmap");
        p.as_mut().data_mut().as_chunks_mut::<4>().0.fill(px);
        p
    }

    fn tile(bytes_side: u32) -> TileImage {
        to_render_image(sized_pixmap(
            PixelFormat::Bgra8Premultiplied,
            [1, 2, 3, 255],
            bytes_side,
            bytes_side,
        ))
    }

    #[test]
    fn opaque_bgra_tiles_are_moved_unchanged() {
        let image = to_render_image(pixmap(PixelFormat::Bgra8Premultiplied, [10, 20, 30, 255]));
        assert_eq!(
            image.as_bytes(0),
            Some(&[10, 20, 30, 255, 10, 20, 30, 255][..])
        );
    }

    #[test]
    fn rgba_is_swizzled_and_alpha_unpremultiplied() {
        // Premultiplied RGBA (100, 50, 0) at alpha 128 -> straight BGRA.
        let image = to_render_image(pixmap(PixelFormat::Rgba8Premultiplied, [100, 50, 0, 128]));
        let bytes = image.as_bytes(0).expect("one frame");
        assert_eq!(&bytes[..4], &[0, 100, 199, 128]);
    }

    #[test]
    fn retire_queue_wakes_only_outside_frames() {
        let queue = RetireQueue::default();
        let image = to_render_image(pixmap(PixelFormat::Bgra8Premultiplied, [0, 0, 0, 255]));
        assert!(queue.retire([Arc::clone(&image)]));
        let woke = queue.in_frame(|| queue.retire([image]));
        assert!(!woke);
        assert!(!queue.is_empty());
        assert_eq!(queue.take().len(), 2);
    }

    #[test]
    fn reserve_spends_the_frame_budget_once_per_image() {
        // 8x8 tiles = 256 bytes; the budget fits two of them.
        let mut t = TileTextures::new(600);
        let (a, b, c) = (tile(8), tile(8), tile(8));
        assert!(t.start_frame(1));
        assert!(t.reserve(&a));
        assert!(t.reserve(&a), "asking twice does not spend twice");
        assert!(t.reserve(&b));
        assert!(!t.reserve(&c), "third upload exceeds the budget");
        assert_eq!(t.stats().deferred, 1);
        t.note_paint(&a);
        t.note_paint(&b);
        // A new frame: resident images are free, c fits now.
        assert!(t.start_frame(2));
        assert!(!t.start_frame(2), "begin_frame is idempotent per frame");
        assert!(t.reserve(&a) && t.reserve(&b) && t.reserve(&c));
        assert_eq!(t.stats().frame_upload_bytes, 256);
    }

    #[test]
    fn the_first_upload_of_a_frame_always_proceeds() {
        let mut t = TileTextures::new(1);
        t.start_frame(1);
        assert!(t.reserve(&tile(8)), "larger than the budget, but first");
        assert!(!t.reserve(&tile(8)));
    }

    #[test]
    fn unreserved_paints_are_tracked_and_forgotten() {
        let mut t = TileTextures::new(1 << 20);
        t.start_frame(1);
        let thumb = tile(4);
        t.note_paint(&thumb);
        t.note_paint(&thumb);
        let s = t.stats();
        assert_eq!((s.resident, s.resident_bytes, s.uploads), (1, 64, 1));
        assert_eq!(s.frame_upload_bytes, 64);
        t.forget(&thumb);
        let s = t.stats();
        assert_eq!((s.resident, s.resident_bytes, s.released), (0, 0, 1));
    }

    #[test]
    fn image_bounds_place_the_source_rect_on_the_destination() {
        // A 516 px tile with a 2 px gutter drawn at 1.5x: the inner 512 px
        // land on a 768 px destination.
        let dest = Bounds::new(point(px(100.0), px(50.0)), size(px(768.0), px(768.0)));
        let b = image_bounds_for(dest, Some([2.0, 2.0, 512.0, 512.0]), (516.0, 516.0));
        assert_eq!(b.origin, point(px(97.0), px(47.0)));
        assert_eq!(b.size, size(px(774.0), px(774.0)));
        assert_eq!(image_bounds_for(dest, None, (516.0, 516.0)), dest);
        assert_eq!(
            image_bounds_for(dest, Some([0.0, 0.0, 0.0, 10.0]), (516.0, 516.0)),
            dest
        );
    }
}
