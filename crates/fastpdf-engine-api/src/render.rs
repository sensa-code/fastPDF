use crate::{PageIndex, PageSize, PixelRect, RenderScale, Rgba8, Rotation};

/// How page colors are mapped. Part of every tile cache key (spec §17).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum ColorMode {
    #[default]
    Normal,
    /// Night mode: colors inverted (white paper becomes black). Applied by
    /// the guard layer after the engine renders normally, so every engine
    /// supports it identically.
    Inverted,
}

/// One render job: a region (usually a tile) of one page at one scale.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderRequest {
    pub page: PageIndex,
    pub scale: RenderScale,
    /// User rotation, applied on top of the page's intrinsic `/Rotate`.
    pub rotation: Rotation,
    /// Region of the rendered page to produce, in pixel space at `scale`
    /// after rotation. A full-page render uses the whole page bounds.
    pub region: PixelRect,
    /// Paper color painted underneath the page content.
    pub background: Rgba8,
    pub color_mode: ColorMode,
    /// Render annotation appearance streams (form fields, stamps, ...).
    pub annotations: bool,
}

impl RenderRequest {
    /// A request covering the whole page.
    ///
    /// `page_size` and `intrinsic` come from the document's
    /// [`crate::PageInfo`]; `rotation` is the user's extra rotation.
    pub fn full_page(
        page: PageIndex,
        page_size: PageSize,
        intrinsic: Rotation,
        rotation: Rotation,
        scale: RenderScale,
    ) -> Self {
        let pixels = scale.page_pixels(page_size, intrinsic.then(rotation));
        Self {
            page,
            scale,
            rotation,
            region: pixels.bounds(),
            background: Rgba8::WHITE,
            color_mode: ColorMode::Normal,
            annotations: true,
        }
    }

    pub fn with_region(mut self, region: PixelRect) -> Self {
        self.region = region;
        self
    }
}

/// Extra information about a finished render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RenderOutcome {
    /// The engine skipped content (budget hit, unsupported feature, broken
    /// object) but produced a usable image.
    pub partial: bool,
}
