//! Translation between zpdf types and the FastPDF domain model.
//!
//! zpdf reports geometry in PDF user space (origin bottom-left, y up, with the
//! CropBox origin wherever the file put it). FastPDF page space is points with
//! the origin at the top-left of the unrotated visible box and y down
//! (`fastpdf_engine_api::PageRect`). Every conversion goes through
//! [`PageGeometry`] so that offset is applied in exactly one place.

use std::time::Duration;

use fastpdf_engine_api::{
    Destination, DestinationView, EngineError, LimitKind, Link, LinkTarget, OutlineItem, PageIndex,
    PageRect, PageSize, ResourceLimits, Rgba8, Rotation, TextSpan,
};
use zpdf_core::{ParseLimits, Rect};

/// zpdf's own anti-hang budget for interpreting one page
/// (`zpdf-content` `INTERPRET_BUDGET`, not configurable).
pub(crate) const ZPDF_INTERPRET_BUDGET: Duration = Duration::from_secs(8);
/// zpdf's display-list command ceiling (`DEFAULT_MAX_COMMANDS`).
pub(crate) const ZPDF_MAX_COMMANDS: usize = 500_000;

/// Retained-object cache of one open document. zpdf defaults to 512 MiB; the
/// cache only admits (never evicts), so a smaller cap degrades to re-parsing
/// instead of holding hundreds of MiB of raw stream copies.
const OBJECT_CACHE_BYTES: u64 = 64 * 1024 * 1024;
/// Decoded object-stream cache (zpdf default 256 MiB).
const OBJSTM_CACHE_BYTES: u64 = 32 * 1024 * 1024;

/// Visible box and intrinsic rotation of one page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PageGeometry {
    /// CropBox ∩ MediaBox in user space, normalized (x0 < x1, y0 < y1).
    pub(crate) visible: Rect,
    pub(crate) rotation: Rotation,
}

impl PageGeometry {
    pub(crate) fn new(visible: Rect, rotate: i32) -> Self {
        Self {
            visible: visible.normalize(),
            // zpdf bakes only exact quarter turns (rem_euclid(360) of 90/180/270)
            // and treats anything else as 0; mirror that.
            rotation: Rotation::from_degrees(i64::from(rotate)).unwrap_or_default(),
        }
    }

    pub(crate) fn size(&self) -> PageSize {
        PageSize::new(self.visible.width() as f32, self.visible.height() as f32)
    }

    pub(crate) fn x(&self, user_x: f64) -> f32 {
        (user_x - self.visible.x0) as f32
    }

    pub(crate) fn y(&self, user_y: f64) -> f32 {
        (self.visible.y1 - user_y) as f32
    }

    /// A user-space rectangle in page space.
    pub(crate) fn rect(&self, r: Rect) -> Option<PageRect> {
        let (x0, y0, x1, y1) = (self.x(r.x0), self.y(r.y0), self.x(r.x1), self.y(r.y1));
        // Check before PageRect::new: its min/max would silently drop a NaN.
        [x0, y0, x1, y1]
            .iter()
            .all(|v| v.is_finite())
            .then(|| PageRect::new(x0, y0, x1, y1))
    }
}

/// Maps a zpdf error to the engine-neutral error.
pub(crate) fn map_error(error: zpdf_core::Error) -> EngineError {
    use zpdf_core::Error as Z;
    match error {
        Z::WrongPassword => EngineError::InvalidPassword,
        Z::RecursionLimit(_) => EngineError::LimitExceeded(LimitKind::Recursion),
        Z::StreamSizeLimit(_) | Z::StringLengthLimit(_) => {
            EngineError::LimitExceeded(LimitKind::ObjectSize)
        }
        Z::UnsupportedFilter(filter) => EngineError::Unsupported(format!("filter {filter}")),
        Z::Io(e) => EngineError::Internal(e.to_string()),
        other => EngineError::Malformed(other.to_string()),
    }
}

pub(crate) fn map_render_error(error: zpdf_render_cpu::CpuRenderError) -> EngineError {
    use zpdf_render_cpu::CpuRenderError as C;
    match error {
        C::InvalidPage(e) => EngineError::InvalidRequest(e.to_string()),
        C::LimitExceeded(_) => EngineError::LimitExceeded(LimitKind::BitmapBytes),
        other => EngineError::Internal(other.to_string()),
    }
}

/// zpdf parse limits for FastPDF's resource limits.
///
/// Where both define a limit the stricter one wins. Two zpdf limits are
/// deliberately *not* lowered: `max_image_cache_bytes` and
/// `max_font_cache_bytes` are per-page admission caps, and zpdf silently drops
/// images / substitutes placeholder fonts past them, so lowering them would
/// blank content instead of saving memory. Retained memory is bounded by the
/// adapter's page cache instead.
pub(crate) fn parse_limits(limits: &ResourceLimits) -> ParseLimits {
    let d = ParseLimits::default();
    ParseLimits {
        max_object_depth: d.max_object_depth.min(limits.max_nesting_depth),
        max_stream_bytes: d.max_stream_bytes.min(limits.max_object_bytes),
        max_decoded_stream_bytes: d.max_decoded_stream_bytes.min(limits.max_object_bytes),
        max_image_pixels: d.max_image_pixels.min(limits.max_decoded_image_pixels),
        // zpdf silently shrinks the scale of a raster above this many pixels.
        // Every target the guard accepts, plus the adapter's raster margin,
        // stays below it, so that never happens.
        max_page_pixels: max_raster_pixels(limits),
        max_object_cache_bytes: OBJECT_CACHE_BYTES,
        max_objstm_cache_bytes: OBJSTM_CACHE_BYTES,
        ..d
    }
}

/// Largest raster the adapter can ask zpdf for: the guard's bitmap cap plus
/// the margin around it.
fn max_raster_pixels(limits: &ResourceLimits) -> u64 {
    let margin = u64::from(crate::prepare::RASTER_MARGIN_PX);
    let side = u64::from(limits.max_bitmap_dimension);
    (limits.max_bitmap_bytes / 4)
        .saturating_add(4 * margin * side)
        .saturating_add(4 * margin * margin)
}

/// Straight 8-bit RGBA to zpdf's float color.
pub(crate) fn color(c: Rgba8) -> zpdf_display_list::Color {
    let f = |v: u8| f32::from(v) / 255.0;
    zpdf_display_list::Color::rgba(f(c.r), f(c.g), f(c.b), f(c.a))
}

/// Converts a zpdf destination; `geometry` looks up the target page.
pub(crate) fn destination(
    dest: &zpdf_document::Destination,
    geometry: &dyn Fn(u32) -> Option<PageGeometry>,
) -> Option<Destination> {
    let page = u32::try_from(dest.page?).ok()?;
    let view = match geometry(page) {
        Some(g) => view(&dest.view, &g),
        // Without the target's box no coordinate can be translated.
        None => DestinationView::Fit,
    };
    Some(Destination {
        page: PageIndex::new(page),
        view,
    })
}

fn view(view: &zpdf_document::DestView, g: &PageGeometry) -> DestinationView {
    use zpdf_document::DestView as V;
    let x = |v: Option<f32>| v.map(|v| g.x(f64::from(v))).filter(|v| v.is_finite());
    let y = |v: Option<f32>| v.map(|v| g.y(f64::from(v))).filter(|v| v.is_finite());
    match *view {
        V::Xyz { left, top, zoom } => DestinationView::Xyz {
            left: x(left),
            top: y(top),
            zoom: zoom.filter(|z| z.is_finite() && *z > 0.0),
        },
        V::Fit | V::FitB | V::Unknown => DestinationView::Fit,
        V::FitH { top } | V::FitBH { top } => DestinationView::FitWidth { top: y(top) },
        V::FitV { left } | V::FitBV { left } => DestinationView::FitHeight { left: x(left) },
        V::FitR {
            left,
            bottom,
            right,
            top,
        } => g
            .rect(Rect::new(
                f64::from(left),
                f64::from(bottom),
                f64::from(right),
                f64::from(top),
            ))
            .map_or(DestinationView::Fit, DestinationView::FitRect),
    }
}

/// Converts an outline tree. zpdf already caps depth (64) and item count
/// (65 536), so the recursion here is bounded.
pub(crate) fn outline(
    items: &[zpdf_document::OutlineItem],
    geometry: &dyn Fn(u32) -> Option<PageGeometry>,
) -> Vec<OutlineItem> {
    items
        .iter()
        .map(|item| OutlineItem {
            title: item.title.clone(),
            destination: item.dest.as_ref().and_then(|d| destination(d, geometry)),
            uri: item.uri.clone(),
            open: item.open,
            children: outline(&item.children, geometry),
        })
        .collect()
}

/// Link annotations of one page.
pub(crate) fn links(
    annotations: &[zpdf_document::Annotation],
    page: &PageGeometry,
    geometry: &dyn Fn(u32) -> Option<PageGeometry>,
) -> Vec<Link> {
    annotations
        .iter()
        .filter(|a| a.subtype == "Link")
        .filter_map(|a| {
            let bounds = page.rect(a.rect)?;
            let target = match (&a.dest, &a.uri) {
                (Some(dest), _) => destination(dest, geometry)
                    .map_or(LinkTarget::Unsupported, LinkTarget::Internal),
                (None, Some(uri)) => LinkTarget::Uri(uri.clone()),
                (None, None) => LinkTarget::Unsupported,
            };
            Some(Link { bounds, target })
        })
        .collect()
}

/// Typical ascender / descender as a fraction of the font size. zpdf spans
/// carry only the baseline origin, size and horizontal advance.
const ASCENT: f64 = 0.85;
const DESCENT: f64 = 0.25;

/// A zpdf text span (baseline origin + horizontal advance, user space) as a
/// page-space span with an approximate box. Per-character boxes are not
/// available from zpdf, so `char_bounds` stays empty.
pub(crate) fn text_span(span: &zpdf_content::text::TextSpan, g: &PageGeometry) -> TextSpan {
    let size = f64::from(span.size).abs();
    let (x0, x1) = (
        span.x.min(span.x + span.advance),
        span.x.max(span.x + span.advance),
    );
    let user = Rect::new(x0, span.y - size * DESCENT, x1, span.y + size * ASCENT);
    TextSpan {
        text: span.text.clone(),
        bounds: g.rect(user).unwrap_or_default(),
        char_bounds: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry() -> PageGeometry {
        // CropBox with a non-zero origin, as in AutoCAD exports.
        PageGeometry::new(Rect::new(100.0, 50.0, 700.0, 850.0), -90)
    }

    #[test]
    fn page_geometry_normalizes_rotation_and_size() {
        let g = geometry();
        assert_eq!(g.rotation, Rotation::R270);
        assert_eq!(g.size(), PageSize::new(600.0, 800.0));
        assert_eq!(
            PageGeometry::new(Rect::new(0.0, 0.0, 10.0, 10.0), 45).rotation,
            Rotation::R0
        );
    }

    #[test]
    fn user_space_maps_to_top_left_page_space() {
        let g = geometry();
        assert_eq!(
            g.rect(Rect::new(100.0, 850.0, 200.0, 800.0)),
            Some(PageRect::new(0.0, 0.0, 100.0, 50.0))
        );
        assert_eq!(g.rect(Rect::new(f64::NAN, 0.0, 1.0, 1.0)), None);
    }

    #[test]
    fn destinations_translate_coordinates() {
        let g = geometry();
        let lookup = move |_: u32| Some(g);
        let dest = zpdf_document::Destination {
            page: Some(3),
            page_ref: None,
            view: zpdf_document::DestView::Xyz {
                left: Some(150.0),
                top: Some(800.0),
                zoom: Some(0.0),
            },
        };
        assert_eq!(
            destination(&dest, &lookup),
            Some(Destination {
                page: PageIndex::new(3),
                view: DestinationView::Xyz {
                    left: Some(50.0),
                    top: Some(50.0),
                    zoom: None,
                },
            })
        );
        let fit_r = zpdf_document::Destination {
            page: Some(0),
            page_ref: None,
            view: zpdf_document::DestView::FitR {
                left: 100.0,
                bottom: 450.0,
                right: 400.0,
                top: 850.0,
            },
        };
        assert_eq!(
            destination(&fit_r, &lookup).map(|d| d.view),
            Some(DestinationView::FitRect(PageRect::new(
                0.0, 0.0, 300.0, 400.0
            )))
        );
        let remote = zpdf_document::Destination {
            page: None,
            page_ref: None,
            view: zpdf_document::DestView::Fit,
        };
        assert_eq!(destination(&remote, &lookup), None);
    }

    #[test]
    fn limits_take_the_stricter_value() {
        let limits = ResourceLimits {
            max_nesting_depth: 10,
            max_object_bytes: 1024,
            ..ResourceLimits::default()
        };
        let p = parse_limits(&limits);
        assert_eq!(p.max_object_depth, 10);
        assert_eq!(p.max_stream_bytes, 1024);
        assert_eq!(p.max_decoded_stream_bytes, 1024);
        assert!(p.max_page_pixels > limits.max_bitmap_bytes / 4);
        // Per-page admission caps are left at zpdf's defaults on purpose.
        assert_eq!(
            p.max_image_cache_bytes,
            ParseLimits::default().max_image_cache_bytes
        );
    }

    #[test]
    fn errors_map_to_engine_errors() {
        assert_eq!(
            map_error(zpdf_core::Error::WrongPassword),
            EngineError::InvalidPassword
        );
        assert_eq!(
            map_error(zpdf_core::Error::RecursionLimit(5)),
            EngineError::LimitExceeded(LimitKind::Recursion)
        );
        assert!(matches!(
            map_error(zpdf_core::Error::NotAPdf),
            EngineError::Malformed(_)
        ));
    }
}
