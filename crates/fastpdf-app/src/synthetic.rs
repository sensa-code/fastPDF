//! Development-only engine (`--features engine-synthetic`,
//! `--engine synthetic`): numbered test pages instead of PDF content.
//!
//! It lets the UI be developed and measured without a real engine: pages
//! carry a large page number, ruled "text" lines, a frame and diagonal
//! stripes that make tile seams and misplaced tiles obvious. The page count
//! comes from a byte scan for page objects (no parsing); every tenth page is
//! landscape to exercise mixed page sizes, and pages 3, 28, 53, ... fail to
//! render to exercise per-page error display (spec §24).

use fastpdf_engine_api::{
    CancelToken, DocumentSource, EngineCapabilities, EngineDocument, EngineError, EngineInfo,
    OpenOptions, PageIndex, PageInfo, PageSize, PdfEngine, PixmapMut, RenderOutcome, RenderRequest,
    Rgba8, Rotation,
};

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SyntheticEngine;

impl PdfEngine for SyntheticEngine {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            name: "synthetic",
            version: "0",
            capabilities: EngineCapabilities {
                region_render: true,
                parallel_render: true,
                cooperative_cancel: true,
                ..EngineCapabilities::default()
            },
        }
    }

    fn open(
        &self,
        source: DocumentSource,
        _options: &OpenOptions,
    ) -> Result<Box<dyn EngineDocument>, EngineError> {
        let pages = count_page_objects(source.data.as_slice());
        Ok(Box::new(SyntheticDocument {
            pages: if pages == 0 { 200 } else { pages.min(100_000) },
        }))
    }
}

/// Counts `/Type /Page` (not `/Pages`) occurrences.
fn count_page_objects(bytes: &[u8]) -> u32 {
    let mut count = 0u32;
    let mut i = 0;
    while let Some(pos) = find(&bytes[i..], b"/Type") {
        let mut j = i + pos + 5;
        while bytes.get(j).is_some_and(u8::is_ascii_whitespace) {
            j += 1;
        }
        if bytes[j..].starts_with(b"/Page") && bytes.get(j + 5) != Some(&b's') {
            count = count.saturating_add(1);
        }
        i = j;
    }
    count
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

struct SyntheticDocument {
    pages: u32,
}

impl EngineDocument for SyntheticDocument {
    fn page_count(&self) -> u32 {
        self.pages
    }

    fn page_info(&self, page: PageIndex) -> Result<PageInfo, EngineError> {
        let size = if page.get() % 10 == 9 {
            PageSize::new(792.0, 612.0)
        } else {
            PageSize::LETTER
        };
        Ok(PageInfo {
            size,
            rotation: Rotation::R0,
        })
    }

    fn render(
        &self,
        request: &RenderRequest,
        target: &mut PixmapMut<'_>,
        cancel: &CancelToken,
    ) -> Result<RenderOutcome, EngineError> {
        if request.page.get() % 25 == 2 {
            return Err(EngineError::Malformed(
                "synthetic broken page (every 25th page from page 3)".into(),
            ));
        }
        let info = self.page_info(request.page)?;
        let page_px = request
            .scale
            .page_pixels(info.size, info.rotation.then(request.rotation));
        let s = request.scale.get();
        let format = target.format();
        let region = request.region;
        let mut canvas = Canvas {
            data: target.data_mut(),
            x0: i64::from(region.x),
            y0: i64::from(region.y),
            width: i64::from(region.width),
            height: i64::from(region.height),
        };
        canvas.fill_all(request.background.premultiplied_bytes(format));

        let (pw, ph) = (i64::from(page_px.width), i64::from(page_px.height));
        let unit = |v: f32| (v * s).round() as i64;
        let ink = Rgba8::new(40, 40, 40, 255).premultiplied_bytes(format);
        let rule = Rgba8::new(170, 178, 190, 255).premultiplied_bytes(format);
        let stripe = Rgba8::new(214, 230, 250, 255).premultiplied_bytes(format);

        // Diagonal stripes across the lower half: seams would break them.
        let band = unit(24.0).max(2);
        canvas.fill_where(0, ph / 2, pw, ph, |x, y| ((x + y) / band) % 2 == 0, stripe);
        cancel.check()?;

        // Ruled "text" lines with varying lengths.
        let margin = unit(54.0);
        let line_gap = unit(16.0).max(2);
        let line_height = unit(6.0).max(1);
        let mut y = unit(190.0);
        let mut n = request.page.get().wrapping_mul(7);
        while y + line_height < ph / 2 {
            n = n.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let len = (pw - 2 * margin) * i64::from(60 + (n >> 16) % 40) / 100;
            canvas.fill_rect(margin, y, margin + len, y + line_height, rule);
            y += line_gap;
        }
        cancel.check()?;

        // Page number, 7-segment style.
        let digit_h = unit(96.0).max(7);
        draw_number(
            &mut canvas,
            request.page.display_number(),
            margin,
            unit(60.0),
            digit_h,
            ink,
        );

        // Frame.
        let t = unit(3.0).max(1);
        canvas.fill_rect(0, 0, pw, t, ink);
        canvas.fill_rect(0, ph - t, pw, ph, ink);
        canvas.fill_rect(0, 0, t, ph, ink);
        canvas.fill_rect(pw - t, 0, pw, ph, ink);
        Ok(RenderOutcome::default())
    }
}

/// A render target positioned at `(x0, y0)` in page pixel space; all
/// drawing is clipped to it.
struct Canvas<'a> {
    data: &'a mut [u8],
    x0: i64,
    y0: i64,
    width: i64,
    height: i64,
}

impl Canvas<'_> {
    fn fill_all(&mut self, px: [u8; 4]) {
        self.data.as_chunks_mut::<4>().0.fill(px);
    }

    /// Fills page-space rectangle `[x0, x1) x [y0, y1)`.
    fn fill_rect(&mut self, x0: i64, y0: i64, x1: i64, y1: i64, px: [u8; 4]) {
        self.fill_where(x0, y0, x1, y1, |_, _| true, px);
    }

    fn fill_where(
        &mut self,
        x0: i64,
        y0: i64,
        x1: i64,
        y1: i64,
        inside: impl Fn(i64, i64) -> bool,
        px: [u8; 4],
    ) {
        let cx0 = x0.max(self.x0);
        let cy0 = y0.max(self.y0);
        let cx1 = x1.min(self.x0 + self.width);
        let cy1 = y1.min(self.y0 + self.height);
        for y in cy0..cy1 {
            let row = ((y - self.y0) * self.width) as usize * 4;
            for x in cx0..cx1 {
                if inside(x, y) {
                    let i = row + ((x - self.x0) as usize) * 4;
                    if let Some(dst) = self.data.get_mut(i..i + 4) {
                        dst.copy_from_slice(&px);
                    }
                }
            }
        }
    }
}

/// Segments a..g of each digit, as bits 0..6.
const SEGMENTS: [u8; 10] = [
    0b011_1111, 0b000_0110, 0b101_1011, 0b100_1111, 0b110_0110, 0b110_1101, 0b111_1101, 0b000_0111,
    0b111_1111, 0b110_1111,
];

fn draw_number(canvas: &mut Canvas<'_>, number: u32, x: i64, y: i64, h: i64, px: [u8; 4]) {
    let w = h / 2;
    let t = (h / 9).max(1);
    let mut cursor = x;
    for digit in number.to_string().bytes().map(|b| usize::from(b - b'0')) {
        let bits = SEGMENTS[digit.min(9)];
        let mid = y + h / 2;
        let rects = [
            (cursor, y, cursor + w, y + t),                     // a: top
            (cursor + w - t, y, cursor + w, mid),               // b: upper right
            (cursor + w - t, mid, cursor + w, y + h),           // c: lower right
            (cursor, y + h - t, cursor + w, y + h),             // d: bottom
            (cursor, mid, cursor + t, y + h),                   // e: lower left
            (cursor, y, cursor + t, mid),                       // f: upper left
            (cursor, mid - t / 2, cursor + w, mid + t - t / 2), // g: middle
        ];
        for (i, (x0, y0, x1, y1)) in rects.into_iter().enumerate() {
            if bits & (1 << i) != 0 {
                canvas.fill_rect(x0, y0, x1, y1, px);
            }
        }
        cursor += w + t * 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_page_objects_not_page_trees() {
        let pdf = b"<< /Type /Pages /Count 2 >> << /Type /Page >> << /Type/Page/Parent 1 0 R >>";
        assert_eq!(count_page_objects(pdf), 2);
        assert_eq!(count_page_objects(b"not a pdf"), 0);
        assert_eq!(count_page_objects(b"/Type"), 0);
    }
}
