//! GPU texture lifetime for tiles (docs/audit/gpui.md, "Texture & Image
//! Pipeline").
//!
//! GPUI uploads an `Arc<RenderImage>` into its sprite atlas the first time it
//! is painted and keeps it there until `Window::drop_image` is called —
//! dropping the `Arc` alone leaks the atlas space for the lifetime of the
//! window. This module makes that lifetime explicit:
//!
//! * Tiles evicted by the tile cache (budget, memory pressure, document
//!   close) are pushed into a [`RetireQueue`] from whatever thread evicts
//!   them; the UI thread releases them with `drop_image` at the start of the
//!   next paint, before anything new is uploaded.
//! * Every image painted at least once is tracked as *resident*, so closing
//!   a document can release all of them even if an eviction were missed.
//! * Uploads are budgeted per frame: an image that would push the frame's
//!   uploads past the budget is skipped this frame (its stand-in or the
//!   paper shows instead) and the caller requests another frame.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use fastpdf_engine_api::{PixelFormat, Pixmap};
use gpui::{Bounds, Corners, ImageId, Pixels, RenderImage, Window};
use image::{Frame, RgbaImage};

/// What a cached tile is in the UI: an image GPUI can paint.
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
    /// Set while the UI thread is inside `DocumentSession::frame`; evictions
    /// during that call are released by the paint that immediately follows,
    /// so they need no extra wake-up.
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

    pub(crate) fn set_in_frame(&self, in_frame: bool) {
        self.inner.in_frame.store(in_frame, Ordering::Release);
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
    /// Uploads postponed by the per-frame budget.
    pub deferred: u64,
    pub frame_upload_bytes: usize,
}

/// Tracks which tile images live in GPUI's sprite atlas.
pub(crate) struct TileTextures {
    retire: RetireQueue,
    resident: HashMap<ImageId, TileImage>,
    resident_bytes: usize,
    upload_budget: usize,
    frame_uploaded: usize,
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
    pub(crate) fn new(upload_budget: usize) -> Self {
        Self {
            retire: RetireQueue::default(),
            resident: HashMap::new(),
            resident_bytes: 0,
            upload_budget: upload_budget.max(1),
            frame_uploaded: 0,
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

    /// Releases retired images and resets the frame's upload budget. Call
    /// at the start of every paint, before [`Self::paint`].
    pub(crate) fn begin_frame(&mut self, window: &mut Window) {
        self.frame_uploaded = 0;
        for image in self.retire.take() {
            self.release(&image, window);
        }
    }

    fn release(&mut self, image: &TileImage, window: &mut Window) {
        // Images that were never painted have no atlas entry.
        if let Some(resident) = self.resident.remove(&image.id) {
            self.resident_bytes = self.resident_bytes.saturating_sub(image_bytes(&resident));
            if let Err(e) = window.drop_image(resident) {
                log::warn!("drop_image failed: {e}");
            }
            self.stats.released += 1;
        }
    }

    /// Paints `image` over `bounds`. Returns false when the upload was
    /// deferred to a later frame by the budget (nothing was painted). The
    /// first upload of a frame always proceeds so progress is guaranteed.
    pub(crate) fn paint(
        &mut self,
        image: &TileImage,
        bounds: Bounds<Pixels>,
        window: &mut Window,
    ) -> bool {
        if !self.resident.contains_key(&image.id) {
            let bytes = image_bytes(image);
            if self.frame_uploaded > 0 && self.frame_uploaded + bytes > self.upload_budget {
                self.stats.deferred += 1;
                return false;
            }
            self.frame_uploaded += bytes;
            self.resident_bytes += bytes;
            self.resident.insert(image.id, Arc::clone(image));
            self.stats.uploads += 1;
            self.stats.upload_bytes += bytes as u64;
        }
        if let Err(e) = window.paint_image(
            bounds,
            bounds,
            Corners::default(),
            Arc::clone(image),
            0,
            false,
        ) {
            log::warn!("paint_image failed: {e}");
        }
        true
    }

    /// Releases every image this window uploaded (document closed).
    pub(crate) fn release_all(&mut self, window: &mut Window) {
        for image in self.retire.take() {
            self.release(&image, window);
        }
        let resident: Vec<TileImage> = self.resident.values().cloned().collect();
        for image in resident {
            self.release(&image, window);
        }
        debug_assert!(self.resident.is_empty());
        self.resident_bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastpdf_engine_api::{PixelSize, ResourceLimits};

    fn pixmap(format: PixelFormat, px: [u8; 4]) -> Pixmap {
        let mut p = Pixmap::new(PixelSize::new(2, 1), format, &ResourceLimits::default())
            .expect("tiny pixmap");
        p.as_mut().data_mut().as_chunks_mut::<4>().0.fill(px);
        p
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
        queue.set_in_frame(true);
        assert!(!queue.retire([image]));
        assert_eq!(queue.take().len(), 2);
    }
}
