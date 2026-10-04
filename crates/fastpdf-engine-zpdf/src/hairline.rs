//! Seamless thin lines across tile borders (see [`HairlinePolicy`]).

use zpdf_core::{Point, Rect};
use zpdf_display_list::{DisplayList, Path, PathElement, RenderCommand, StrokeStyle};

/// Device width given to widened hairline segments; above one pixel, so
/// tiny-skia strokes them with its regular (window-independent) path filler.
const MIN_STROKE_PX: f32 = 1.01;

/// Distance from the raster edge, in device pixels, within which tiny-skia's
/// hairline rasterizer clips a segment (it pads the segment's integer bounds
/// by one pixel before testing them against the raster).
const HAIRLINE_CLIP_BAND_PX: f64 = 2.0;

/// Spacing of the page-pixel grid on which tile borders are expected to lie
/// (`fastpdf-render` tiles are 512 px by default, starting at the page
/// origin). Regions not aligned to it fall back to a per-raster decision.
pub(crate) const HAIRLINE_GRID_PX: u32 = 256;

/// Keeps thin lines seamless across tile borders.
///
/// zpdf draws every stroke at least one device pixel wide, and tiny-skia
/// rasterizes strokes of at most one pixel with its hairline algorithm. That
/// algorithm is translation invariant for segments it draws whole, but a
/// segment it has to *clip* is stepped from the clipped end point, so the
/// same segment can land up to a pixel apart in two rasters that clip it
/// differently: a visible step where a thin line crosses a tile border
/// (reproduced with tiny-skia alone).
///
/// A hairline segment is therefore drawn 1.01 px wide through the path
/// filler, which is window independent, when it crosses a line of the
/// page-pixel grid (where tile borders lie) or would be clipped by this
/// raster; every other segment stays a hairline. Deciding by the page grid
/// rather than by the raster alone makes a segment look the same in every
/// tile and in a full-page render.
///
/// Cost, measured with `fastpdf-bench render` on the A0 fixtures at 96 dpi:
/// none on a dense polyline map, +15% (full page) / +25% (512 px tiles) on a
/// hairline mesh, but 2.4x / 1.9x on a floor plan made of long hairlines,
/// which cross grid lines everywhere. Widening every hairline instead was
/// 1.3-3.2x slower across all three.
pub(crate) struct HairlinePolicy {
    /// Hairline segments inside this rectangle are never clipped by tiny-skia;
    /// `None` decides by the page grid alone (grid-aligned regions).
    unclipped: Option<Rect>,
    page_x0: f64,
    page_y1: f64,
    scale: f64,
    scale_f32: f32,
}

impl HairlinePolicy {
    /// Decides by the page grid alone. Valid for regions whose borders lie
    /// on the grid (or on the page edge), and identical for every such
    /// region, so its replacements can be cached per scale.
    pub(crate) fn for_page(page: &Rect, scale: f32) -> Self {
        Self {
            unclipped: None,
            page_x0: page.x0,
            page_y1: page.y1,
            scale: f64::from(scale),
            scale_f32: scale,
        }
    }

    /// Also widens whatever this particular raster would clip, for regions
    /// that are not aligned to the grid.
    pub(crate) fn for_raster(raster: &Rect, page: &Rect, scale: f32) -> Self {
        let band = HAIRLINE_CLIP_BAND_PX / f64::from(scale);
        Self {
            unclipped: Some(Rect::new(
                raster.x0 + band,
                raster.y0 + band,
                raster.x1 - band,
                raster.y1 - band,
            )),
            ..Self::for_page(page, scale)
        }
    }

    fn is_hairline(&self, style: &StrokeStyle) -> bool {
        // zpdf turns a NaN width into a hairline too.
        let width = style.width * self.scale_f32;
        width.is_nan() || width <= 1.0
    }

    fn widened(&self, style: &StrokeStyle) -> StrokeStyle {
        StrokeStyle {
            width: MIN_STROKE_PX / self.scale_f32,
            ..style.clone()
        }
    }

    /// True when the segment from `from` through `points` must not be drawn
    /// as a hairline.
    fn needs_widening(&self, from: Point, points: &[Point]) -> bool {
        let mut x0 = f64::INFINITY;
        let mut x1 = f64::NEG_INFINITY;
        let mut y0 = f64::INFINITY;
        let mut y1 = f64::NEG_INFINITY;
        for p in std::iter::once(&from).chain(points) {
            if let Some(r) = &self.unclipped
                && !(p.x >= r.x0 && p.x <= r.x1 && p.y >= r.y0 && p.y <= r.y1)
            {
                return true;
            }
            x0 = x0.min(p.x);
            x1 = x1.max(p.x);
            y0 = y0.min(p.y);
            y1 = y1.max(p.y);
        }
        // Page-pixel grid cells of the segment's extent (y grows downwards).
        let grid = f64::from(HAIRLINE_GRID_PX);
        let cell = |v: f64| (v * self.scale / grid).floor();
        cell(x0 - self.page_x0) != cell(x1 - self.page_x0)
            || cell(self.page_y1 - y1) != cell(self.page_y1 - y0)
    }

    /// The commands to draw instead of `command`, or `None` to draw it as is.
    pub(crate) fn replace(&self, command: &RenderCommand) -> Option<Vec<RenderCommand>> {
        match command {
            RenderCommand::StrokePath {
                path,
                style,
                paint,
                alpha,
                overprint,
            } if self.is_hairline(style) => {
                let stroke = |path: Path, style: StrokeStyle| RenderCommand::StrokePath {
                    path,
                    style,
                    paint: paint.clone(),
                    alpha: *alpha,
                    overprint: *overprint,
                };
                if style.dash.is_some() {
                    // Splitting would restart the dash pattern: all or nothing.
                    return self
                        .any_segment_needs_widening(path)
                        .then(|| vec![stroke(path.clone(), self.widened(style))]);
                }
                let (thin, widened) = self.split(path)?;
                let mut commands = Vec::with_capacity(2);
                if !thin.is_empty() {
                    commands.push(stroke(thin, style.clone()));
                }
                if !widened.is_empty() {
                    commands.push(stroke(widened, self.widened(style)));
                }
                Some(commands)
            }
            // A clip must stay one command (its PopClip pops once).
            RenderCommand::PushClipStroke { path, style }
                if self.is_hairline(style) && self.any_segment_needs_widening(path) =>
            {
                Some(vec![RenderCommand::PushClipStroke {
                    path: path.clone(),
                    style: self.widened(style),
                }])
            }
            _ => None,
        }
    }

    fn any_segment_needs_widening(&self, path: &Path) -> bool {
        let mut found = false;
        for_each_segment(path, |from, points| {
            found = found || self.needs_widening(from, points);
        });
        found
    }

    /// Splits a hairline path into segments that stay hairlines and segments
    /// to widen; `None` when nothing needs widening. Hairlines have no joins
    /// and each segment is drawn on its own, so regrouping segments does not
    /// change the result.
    fn split(&self, path: &Path) -> Option<(Path, Path)> {
        let mut thin = Path::new();
        let mut widened = Path::new();
        let mut thin_end: Option<Point> = None;
        let mut widened_end: Option<Point> = None;
        let mut any_widened = false;
        for_each_segment(path, |from, points| {
            let widen = self.needs_widening(from, points);
            any_widened |= widen;
            let (target, end) = if widen {
                (&mut widened, &mut widened_end)
            } else {
                (&mut thin, &mut thin_end)
            };
            if *end != Some(from) {
                target.move_to(from);
            }
            match *points {
                [p] => target.line_to(p),
                [c1, c2, p] => target.curve_to(c1, c2, p),
                _ => {}
            }
            *end = points.last().copied();
        });
        any_widened.then_some((thin, widened))
    }
}

/// Calls `f(start, rest)` for every drawn segment of `path`: `rest` is one
/// end point for a line or three points for a cubic. `Close` yields the line
/// back to the subpath start.
fn for_each_segment(path: &Path, mut f: impl FnMut(Point, &[Point])) {
    let mut start: Option<Point> = None;
    let mut current: Option<Point> = None;
    for element in &path.elements {
        match *element {
            PathElement::MoveTo(p) => {
                start = Some(p);
                current = Some(p);
            }
            PathElement::LineTo(p) => {
                if let Some(from) = current {
                    f(from, &[p]);
                }
                current = Some(p);
            }
            PathElement::CurveTo(c1, c2, p) => {
                if let Some(from) = current {
                    f(from, &[c1, c2, p]);
                }
                current = Some(p);
            }
            PathElement::Close => {
                if let (Some(from), Some(first)) = (current, start)
                    && from != first
                {
                    f(from, &[first]);
                }
                current = start;
            }
        }
    }
}

/// Hairline replacements of one display list at one scale, by command index.
pub(crate) struct Replacements {
    by_command: Vec<Option<Box<[RenderCommand]>>>,
    bytes: u64,
}

impl Replacements {
    /// Applies the grid-only policy to every command of `dl`.
    pub(crate) fn compute(dl: &DisplayList, scale: f32) -> Self {
        let policy = HairlinePolicy::for_page(&dl.page_rect, scale);
        let mut bytes = 0u64;
        let by_command = dl
            .commands
            .iter()
            .map(|command| {
                let replaced = policy.replace(command)?;
                bytes = bytes.saturating_add(commands_bytes(&replaced));
                Some(replaced.into_boxed_slice())
            })
            .collect();
        Self { by_command, bytes }
    }

    pub(crate) fn get(&self, index: usize) -> Option<&[RenderCommand]> {
        self.by_command.get(index)?.as_deref()
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }
}

fn commands_bytes(commands: &[RenderCommand]) -> u64 {
    let element = std::mem::size_of::<PathElement>() as u64;
    commands
        .iter()
        .map(|c| {
            let path = match c {
                RenderCommand::StrokePath { path, .. }
                | RenderCommand::PushClipStroke { path, .. } => path.elements.len() as u64,
                _ => 0,
            };
            std::mem::size_of::<RenderCommand>() as u64 + path * element
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use zpdf_display_list::{Color, Paint};

    fn hairline(points: &[(f64, f64)]) -> RenderCommand {
        let mut path = Path::new();
        for (i, (x, y)) in points.iter().enumerate() {
            let p = Point::new(*x, *y);
            if i == 0 {
                path.move_to(p)
            } else {
                path.line_to(p)
            }
        }
        RenderCommand::StrokePath {
            path,
            style: StrokeStyle {
                width: 0.0,
                ..StrokeStyle::default()
            },
            paint: Paint::Solid(Color::black()),
            alpha: 1.0,
            overprint: None,
        }
    }

    fn widths(commands: &[RenderCommand]) -> Vec<(usize, f32)> {
        commands
            .iter()
            .map(|c| match c {
                RenderCommand::StrokePath { path, style, .. } => (path.elements.len(), style.width),
                _ => (0, -1.0),
            })
            .collect()
    }

    #[test]
    fn only_segments_crossing_the_grid_are_widened() {
        // Page 1000 x 1000 points at scale 1: grid lines at x/y = 256, 512, ...
        let page = Rect::new(0.0, 0.0, 1000.0, 1000.0);
        let policy = HairlinePolicy::for_page(&page, 1.0);
        // Inside one cell: drawn as is.
        assert!(
            policy
                .replace(&hairline(&[(10.0, 990.0), (100.0, 900.0)]))
                .is_none()
        );
        // Second segment crosses x = 256: split into a thin and a widened path.
        let split = policy
            .replace(&hairline(&[(10.0, 990.0), (200.0, 990.0), (300.0, 990.0)]))
            .unwrap();
        assert_eq!(widths(&split), vec![(2, 0.0), (2, 1.01)]);
        // A thick stroke is never touched.
        let mut thick = hairline(&[(10.0, 990.0), (300.0, 990.0)]);
        if let RenderCommand::StrokePath { style, .. } = &mut thick {
            style.width = 2.0;
        }
        assert!(policy.replace(&thick).is_none());
    }

    #[test]
    fn raster_policy_also_widens_clipped_segments() {
        let page = Rect::new(0.0, 0.0, 1000.0, 1000.0);
        let raster = Rect::new(0.0, 900.0, 100.0, 1000.0);
        let policy = HairlinePolicy::for_raster(&raster, &page, 1.0);
        // Leaves the raster (x > 98) without crossing the grid.
        let replaced = policy
            .replace(&hairline(&[(10.0, 950.0), (150.0, 950.0)]))
            .unwrap();
        assert_eq!(widths(&replaced), vec![(2, 1.01)]);
        assert!(
            policy
                .replace(&hairline(&[(10.0, 950.0), (50.0, 950.0)]))
                .is_none()
        );
    }
}
