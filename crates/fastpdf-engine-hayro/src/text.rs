//! Text layer: glyph Unicode and geometry from hayro's interpreter, grouped
//! into spans with simple heuristics (docs/audit/hayro.md, "Text Extraction
//! Feasibility"). hayro reports glyphs in content-stream order with their
//! Unicode (ToUnicode → glyph name → `uniXXXX`; UCS2 CMaps for non-embedded
//! CID fonts) and full transforms; layout analysis is ours.

use fastpdf_engine_api::{EngineError, PageIndex, PageRect, TextLayer, TextSpan};
use hayro::hayro_interpret::font::{Glyph, GlyphRun};
use hayro::hayro_interpret::hayro_cmap::BfString;
use hayro::hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps,
    InterpreterSettings, SoftMask, interpret_page,
};
use hayro::kurbo::{Affine, BezPath, Point, Rect, Vec2};

use crate::document::{DocInner, Generation};
use crate::fonts;
use crate::geometry::{PageGeom, to_f32};
use crate::pool::{TextJob, ThreadCaches};
use crate::preflight::{self, Abort, Budget, run_budgeted};

/// Glyph box in glyph space (1000 units per em): descender and ascender
/// used for every font, since hayro does not expose font metrics.
const DESCENT: f64 = -200.0;
const ASCENT: f64 = 800.0;
/// Fallback advance when a font reports none (Type3, broken widths).
const DEFAULT_ADVANCE: f64 = 500.0;
/// Gap (in em) above which a space is inserted between glyphs of a span.
const SPACE_GAP_EM: f64 = 0.22;
/// Gap (in em) above which the next glyph starts a new span.
const SPAN_BREAK_EM: f64 = 3.0;
/// Baseline offset (in em) still considered the same line.
const BASELINE_TOLERANCE_EM: f64 = 0.35;
/// Recently finished spans compared for duplicates (fake bold / shadows).
const DEDUP_WINDOW: usize = 4;
/// Upper bound on glyphs collected for one page.
const MAX_GLYPHS: usize = 1_000_000;

pub(crate) fn execute_text<'p>(
    doc: &DocInner,
    generation: &'p Generation,
    caches: &mut ThreadCaches<'p>,
    job: &TextJob,
) -> Result<TextLayer, EngineError> {
    job.cancel.check()?;
    let pages = generation.pdf.pages();
    let page = pages
        .get(job.page as usize)
        .ok_or_else(|| EngineError::Internal("page index out of range".into()))?;
    preflight::ensure_verdict(doc, job.page, page, &caches.interp, Some(&job.cancel))?;

    let geom = PageGeom::from_page(page);
    let size = geom.page_size();
    let settings = InterpreterSettings {
        font_resolver: fonts::resolver(),
        render_annotations: false,
        ..InterpreterSettings::default()
    };
    let mut collector = TextCollector {
        budget: Budget::new(doc.limits.max_render_time, Some(job.cancel.clone())),
        builder: SpanBuilder::default(),
        glyphs: 0,
    };
    let interp = &caches.interp;
    let result = run_budgeted(|| {
        let mut ctx = Context::new(
            geom.user_to_page(),
            Rect::new(0.0, 0.0, f64::from(size.width), f64::from(size.height)),
            interp,
            page.xref(),
            settings,
        );
        interpret_page(page, &mut ctx, &mut collector);
    });
    match result {
        Ok(()) => {}
        Err(Abort::Cancelled) => return Err(EngineError::Cancelled),
        Err(Abort::Budget) => {
            return Err(EngineError::LimitExceeded(
                fastpdf_engine_api::LimitKind::RenderTime,
            ));
        }
    }
    if let Some(stream) = page.page_stream() {
        doc.account_content(generation, job.page, stream.len());
    }
    caches.note_page(job.page);
    Ok(TextLayer {
        page: PageIndex::new(job.page),
        spans: collector.builder.finish(),
    })
}

struct TextCollector {
    budget: Budget,
    builder: SpanBuilder,
    glyphs: usize,
}

impl<'a> Device<'a> for TextCollector {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {
        self.budget.tick(1);
    }

    fn push_clip_path(&mut self, _: &ClipPath) {
        self.budget.tick(1);
    }

    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {
        self.budget.tick(1);
    }

    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, _: &DrawMode) {
        let glyphs = run.glyphs();
        self.budget.tick(glyphs.len() as u64);
        for glyph in glyphs {
            if self.glyphs >= MAX_GLYPHS {
                return;
            }
            self.glyphs += 1;
            let Some(text) = glyph.as_unicode() else {
                continue;
            };
            let (advance, font) = match &**glyph {
                Glyph::Outline(o) => (
                    o.advance_width().map(f64::from).unwrap_or(DEFAULT_ADVANCE),
                    o.font_cache_key(),
                ),
                Glyph::Type3(_) => (DEFAULT_ADVANCE, 0),
            };
            let text = match text {
                BfString::Char(c) => c.to_string(),
                BfString::String(s) => s,
            };
            self.builder.push(PlacedGlyph {
                text,
                transform: props.transform * glyph.transform(),
                advance: if advance > 0.0 {
                    advance
                } else {
                    DEFAULT_ADVANCE
                },
                font,
            });
        }
    }

    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {
        self.budget.tick(1);
    }

    fn pop_clip(&mut self) {
        self.budget.tick(1);
    }

    fn pop_transparency_group(&mut self) {
        self.budget.tick(1);
    }
}

/// One glyph in page space (points, y down).
struct PlacedGlyph {
    text: String,
    /// Glyph space (1000 units/em) → page space.
    transform: Affine,
    advance: f64,
    font: u128,
}

impl PlacedGlyph {
    fn origin(&self) -> Point {
        self.transform * Point::ZERO
    }

    fn end(&self) -> Point {
        self.transform * Point::new(self.advance, 0.0)
    }

    /// Baseline direction (unit vector) and em size in page space.
    fn frame(&self) -> (Vec2, Vec2, f64) {
        let o = self.origin();
        let x = self.transform * Point::new(1000.0, 0.0) - o;
        let y = self.transform * Point::new(0.0, 1000.0) - o;
        let em = y.hypot();
        let unit = |v: Vec2| {
            let len = v.hypot();
            if len > f64::EPSILON {
                v / len
            } else {
                Vec2::new(1.0, 0.0)
            }
        };
        (unit(x), unit(y), em)
    }

    fn bounds(&self) -> Rect {
        let corners = [
            Point::new(0.0, DESCENT),
            Point::new(self.advance, DESCENT),
            Point::new(self.advance, ASCENT),
            Point::new(0.0, ASCENT),
        ]
        .map(|p| self.transform * p);
        corners
            .iter()
            .skip(1)
            .fold(Rect::from_points(corners[0], corners[0]), |r, p| {
                r.union_pt(*p)
            })
    }
}

#[derive(Default)]
struct SpanBuilder {
    spans: Vec<TextSpan>,
    current: Option<OpenSpan>,
}

struct OpenSpan {
    span: TextSpan,
    font: u128,
    em: f64,
    last: PlacedGlyph,
}

/// How the next glyph relates to the previous one.
enum Flow {
    Continue { space: Option<Rect> },
    Break,
}

impl SpanBuilder {
    fn push(&mut self, glyph: PlacedGlyph) {
        let flow = match &self.current {
            Some(open) => flow(open, &glyph),
            None => Flow::Break,
        };
        match flow {
            Flow::Continue { space } => {
                if let Some(open) = self.current.as_mut() {
                    if let Some(gap) = space {
                        let ends_with_space = open.span.text.ends_with(' ');
                        if !ends_with_space && !glyph.text.starts_with(' ') {
                            open.span.text.push(' ');
                            open.span.char_bounds.push(page_rect(gap));
                        }
                    }
                    append(&mut open.span, &glyph);
                    open.last = glyph;
                }
            }
            Flow::Break => {
                self.close();
                let (_, _, em) = glyph.frame();
                let mut span = TextSpan::default();
                append(&mut span, &glyph);
                self.current = Some(OpenSpan {
                    span,
                    font: glyph.font,
                    em,
                    last: glyph,
                });
            }
        }
    }

    fn close(&mut self) {
        let Some(open) = self.current.take() else {
            return;
        };
        let span = open.span;
        if span.text.trim().is_empty() {
            return;
        }
        let duplicate = self
            .spans
            .iter()
            .rev()
            .take(DEDUP_WINDOW)
            .any(|s| s.text == span.text && overlap(s.bounds, span.bounds) > 0.8);
        if !duplicate {
            self.spans.push(span);
        }
    }

    fn finish(mut self) -> Vec<TextSpan> {
        self.close();
        self.spans
    }
}

fn flow(open: &OpenSpan, next: &PlacedGlyph) -> Flow {
    let (u, v, em) = open.last.frame();
    let (_, _, next_em) = next.frame();
    if open.font != next.font
        || em <= f64::EPSILON
        || (next_em - open.em).abs() > 0.2 * open.em.max(f64::EPSILON)
    {
        return Flow::Break;
    }
    // Horizontal flow: next origin relative to the previous glyph's end.
    let d = next.origin() - open.last.end();
    let along = d.dot(u) / em;
    let across = d.dot(v) / em;
    if across.abs() <= BASELINE_TOLERANCE_EM && (-0.5..=SPAN_BREAK_EM).contains(&along) {
        let space = (along > SPACE_GAP_EM)
            .then(|| Rect::from_points(open.last.end(), next.origin()).inflate(0.0, em * 0.3));
        return Flow::Continue { space };
    }
    // Vertical flow (WMode 1): origins move down the text-space y axis.
    let o = next.origin() - open.last.origin();
    let down = -o.dot(v) / em;
    let side = o.dot(u) / em;
    if side.abs() <= 0.5 && (0.2..=1.6).contains(&down) {
        return Flow::Continue { space: None };
    }
    Flow::Break
}

fn append(span: &mut TextSpan, glyph: &PlacedGlyph) {
    let bounds = glyph.bounds();
    let chars: Vec<char> = glyph.text.chars().collect();
    let n = chars.len().max(1) as f64;
    // Ligatures and multi-char mappings share the glyph box evenly.
    for (i, c) in chars.iter().enumerate() {
        let x0 = bounds.x0 + bounds.width() * i as f64 / n;
        let x1 = bounds.x0 + bounds.width() * (i + 1) as f64 / n;
        span.text.push(*c);
        span.char_bounds
            .push(page_rect(Rect::new(x0, bounds.y0, x1, bounds.y1)));
    }
    let r = page_rect(bounds);
    span.bounds = if span.char_bounds.len() == chars.len() {
        r
    } else {
        span.bounds.union(r)
    };
}

fn page_rect(r: Rect) -> PageRect {
    PageRect::new(to_f32(r.x0), to_f32(r.y0), to_f32(r.x1), to_f32(r.y1))
}

/// Intersection over the smaller area.
fn overlap(a: PageRect, b: PageRect) -> f32 {
    let w = (a.x1.min(b.x1) - a.x0.max(b.x0)).max(0.0);
    let h = (a.y1.min(b.y1) - a.y0.max(b.y0)).max(0.0);
    let smaller = (a.width() * a.height()).min(b.width() * b.height());
    if smaller <= 0.0 { 0.0 } else { w * h / smaller }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 10 pt glyph at (x, y) in page space (y down, text upright).
    fn glyph(text: &str, x: f64, y: f64, advance: f64) -> PlacedGlyph {
        // glyph space (y up, 1000/em) → page space (y down), 10 pt.
        let t = Affine::new([0.01, 0.0, 0.0, -0.01, x, y]);
        PlacedGlyph {
            text: text.to_owned(),
            transform: t,
            advance,
            font: 1,
        }
    }

    fn texts(spans: &[TextSpan]) -> Vec<&str> {
        spans.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn words_and_lines() {
        let mut b = SpanBuilder::default();
        b.push(glyph("H", 10.0, 20.0, 600.0));
        b.push(glyph("i", 16.0, 20.0, 300.0));
        // gap of 4 pt = 0.4 em → space
        b.push(glyph("y", 23.0, 20.0, 500.0));
        // next line
        b.push(glyph("o", 10.0, 35.0, 500.0));
        let spans = b.finish();
        assert_eq!(texts(&spans), vec!["Hi y", "o"]);
        assert_eq!(spans[0].char_bounds.len(), spans[0].text.chars().count());
        let first = spans[0].char_bounds[0];
        assert!(first.y0 < 20.0 && first.y1 > 20.0, "{first}");
    }

    #[test]
    fn duplicates_are_dropped() {
        let mut b = SpanBuilder::default();
        for _ in 0..2 {
            b.push(glyph("授", 10.0, 20.0, 1000.0));
            b.push(glyph("權", 20.0, 20.0, 1000.0));
        }
        assert_eq!(texts(&b.finish()), vec!["授權"]);
    }

    #[test]
    fn vertical_text_stays_in_one_span() {
        let mut b = SpanBuilder::default();
        for (i, c) in ["直", "排", "文"].iter().enumerate() {
            b.push(glyph(c, 50.0, 20.0 + 10.0 * i as f64, 1000.0));
        }
        assert_eq!(texts(&b.finish()), vec!["直排文"]);
    }
}
