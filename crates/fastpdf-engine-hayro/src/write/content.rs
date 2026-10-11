//! What a page's content draws as text, operator by operator, for text
//! edits (ADR 0013).
//!
//! hayro's content parser splits the content into operators. Their byte
//! ranges come from where the parsed operator names (and inline image data)
//! lie in the content buffer, so the editor and the interpreter never
//! disagree about operator boundaries. Glyph positions, advances, Unicode
//! and paint come from hayro's interpreter: the content is interpreted with
//! a marked-content marker around every text-showing operator, which tells
//! the recording device which operator drew each glyph run; glyphs drawn
//! outside a marker come from form XObjects. A marker after every `ET`
//! tells how many glyphs were drawn when each text object ended (new text
//! goes there). The text state an operator runs with (font, size,
//! character and word spacing) is tracked alongside, and how the bytes of
//! its strings split into character codes follows hayro's own rules for
//! the font (`codes`).

use std::ops::Range;
use std::panic::{AssertUnwindSafe, catch_unwind};

use hayro::hayro_interpret::font::{Glyph, GlyphRun};
use hayro::hayro_interpret::hayro_cmap::{self, BfString, CMap, CMapName, WritingMode};
use hayro::hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps,
    InterpreterCache, InterpreterSettings, Paint, SoftMask, interpret,
};
use hayro::hayro_syntax::content::ops::TypedInstruction;
use hayro::hayro_syntax::content::{TypedIter, UntypedIter};
use hayro::hayro_syntax::object::{Dict, Name, Object};
use hayro::hayro_syntax::page::{Page, Resources};
use hayro::kurbo::{Affine, BezPath, Point, Rect};

use crate::fonts;
use crate::geometry::PageGeom;
use crate::text;

/// Tag of the marker around text-showing operator `n`: `FastPdfEditOp<n>`.
const MARKER: &[u8] = b"FastPdfEditOp";
/// Tag of the marker after the `ET` of text object `n`.
const END_MARKER: &[u8] = b"FastPdfEditEnd";
/// Upper bound on glyphs recorded for one page (as for text layers).
const MAX_GLYPHS: usize = 1_000_000;

/// How a text-showing operator moves before it shows its strings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Show {
    /// `Tj` and `TJ`.
    Plain,
    /// `'`: to the next line first.
    NextLine,
    /// `"`: set word and character spacing, then to the next line.
    NextLineSpaced { word: f64, char: f64 },
}

/// A string or a position adjustment of a text-showing operator.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Item {
    Text(Vec<u8>),
    Adjust(f64),
}

/// A text-showing operator of the page's content.
#[derive(Debug, Clone)]
pub(crate) struct TextOp {
    /// The operator with its operands, from the end of the operator before
    /// it.
    pub(crate) range: Range<usize>,
    pub(crate) show: Show,
    pub(crate) items: Vec<Item>,
    /// The text state it runs with: the font's resource name and size, and
    /// the character and word spacing.
    pub(crate) font: Option<Vec<u8>>,
    pub(crate) size: f64,
    pub(crate) char_space: f64,
    pub(crate) word_space: f64,
    /// The glyphs it drew, as a range of [`Analysis::glyphs`].
    pub(crate) glyphs: Range<usize>,
    /// The text object (index into [`Analysis::blocks`]) it is in.
    pub(crate) block: Option<usize>,
    /// A later `Tj` or `TJ` goes on from where it leaves the text position
    /// (nothing moves the position to a new line or ends the text object
    /// in between).
    pub(crate) continues: bool,
}

/// A text object (`BT` ... `ET`) of the page's content.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TextBlock {
    /// Where its `ET` ends; `None` when the content ends inside it.
    pub(crate) end: Option<usize>,
    /// It changes the graphics state stack or the CTM (`q`, `Q`, `cm`,
    /// which the specification does not allow in text objects): after its
    /// `ET`, the CTM may not be the one its text was drawn with.
    pub(crate) changes_state: bool,
    /// How many glyphs the page had drawn when it ended.
    pub(crate) glyphs_before_end: usize,
}

/// How a glyph is painted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Mode {
    Fill,
    Stroke,
    FillStroke,
    Invisible,
}

/// The paint of a glyph: colors as sRGB components, the stroke width in
/// points (page space).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Look {
    pub(crate) mode: Mode,
    pub(crate) fill: Option<[f32; 3]>,
    pub(crate) stroke: Option<[f32; 3]>,
    pub(crate) stroke_width: f64,
}

/// A glyph the page draws.
#[derive(Debug, Clone)]
pub(crate) struct PlacedGlyph {
    /// The text-showing operator (index into [`Analysis::ops`]) that drew
    /// it; `None` for glyphs of form XObjects.
    pub(crate) op: Option<usize>,
    /// Its Unicode text; empty when the font does not tell.
    pub(crate) text: String,
    /// Glyph space (1000 units per em) to page space.
    pub(crate) transform: Affine,
    /// The CTM it was drawn with (user space to page space).
    pub(crate) ctm: Affine,
    /// The advance hayro moves the text position by, in glyph space units
    /// (`None`: the font has none, and hayro moves by 0).
    pub(crate) advance: Option<f64>,
    /// The advance the text layer sizes the glyph's box with.
    pub(crate) layer_advance: f64,
    pub(crate) look: Look,
    pub(crate) type3: bool,
}

impl PlacedGlyph {
    pub(crate) fn origin(&self) -> Point {
        self.transform * Point::ZERO
    }

    /// Where the text position is after the glyph (without character or
    /// word spacing).
    pub(crate) fn end(&self) -> Point {
        self.transform * Point::new(self.advance.unwrap_or(self.layer_advance), 0.0)
    }

    /// The glyph's box as the text layer has it.
    pub(crate) fn bounds(&self) -> Rect {
        text::glyph_bounds(self.transform, self.layer_advance)
    }
}

/// A page's content with its text, operator by operator.
pub(crate) struct Analysis {
    /// The page's content: its streams decoded and joined as hayro reads
    /// them.
    pub(crate) content: Vec<u8>,
    pub(crate) ops: Vec<TextOp>,
    pub(crate) blocks: Vec<TextBlock>,
    /// Every glyph the page draws, in drawing order.
    pub(crate) glyphs: Vec<PlacedGlyph>,
    /// The content restores more graphics states (`Q`) than it saves.
    pub(crate) underflows: bool,
    /// Graphics states the content saves and leaves unrestored.
    pub(crate) open_states: usize,
}

/// Reads `page`'s content and the glyphs it draws. `None` when the content
/// cannot be taken apart reliably (see [`scan`]) or interpreting it fails.
pub(crate) fn analyze(page: &Page<'_>) -> Option<Analysis> {
    let content = page.page_stream().unwrap_or_default().to_vec();
    let Scan {
        mut ops,
        mut blocks,
        underflows,
        open_states,
    } = scan(&content)?;
    let marked = instrument(&content, &ops, &blocks);
    let glyphs = record(page, &marked, &mut ops, &mut blocks)?;
    Some(Analysis {
        content,
        ops,
        blocks,
        glyphs,
        underflows,
        open_states,
    })
}

/// Interprets `content` as `page`'s content and records the glyphs it
/// draws; `ops` learn which glyphs are theirs, `blocks` how many glyphs
/// were drawn when they ended.
fn record(
    page: &Page<'_>,
    content: &[u8],
    ops: &mut [TextOp],
    blocks: &mut [TextBlock],
) -> Option<Vec<PlacedGlyph>> {
    let geom = PageGeom::from_page(page);
    let size = geom.page_size();
    let settings = InterpreterSettings {
        font_resolver: fonts::resolver(),
        render_annotations: false,
        ..InterpreterSettings::default()
    };
    let cache = InterpreterCache::new();
    let mut recorder = Recorder {
        current: None,
        nested: 0,
        runs: vec![None; ops.len()],
        block_ends: vec![None; blocks.len()],
        glyphs: Vec::new(),
    };
    catch_unwind(AssertUnwindSafe(|| {
        let mut ctx = Context::new(
            geom.user_to_page(),
            Rect::new(0.0, 0.0, f64::from(size.width), f64::from(size.height)),
            &cache,
            page.xref(),
            settings,
        );
        interpret(
            TypedIter::new(content),
            page.resources(),
            &mut ctx,
            &mut recorder,
        );
    }))
    .ok()?;
    for (op, run) in ops.iter_mut().zip(&recorder.runs) {
        op.glyphs = run.clone().unwrap_or(0..0);
    }
    for (block, end) in blocks.iter_mut().zip(&recorder.block_ends) {
        match end {
            Some(n) => block.glyphs_before_end = *n,
            // Not reached (hayro stopped earlier): no place for new text.
            None => block.end = None,
        }
    }
    Some(recorder.glyphs)
}

/// Text state values the editor needs. `q` saves and `Q` restores them
/// with the rest of the graphics state.
#[derive(Debug, Clone, PartialEq)]
struct TextParams {
    font: Option<Vec<u8>>,
    size: f64,
    char_space: f64,
    word_space: f64,
}

impl Default for TextParams {
    /// hayro's initial text state.
    fn default() -> Self {
        Self {
            font: None,
            size: 1.0,
            char_space: 0.0,
            word_space: 0.0,
        }
    }
}

/// The text-showing operators of some content.
pub(crate) struct Scan {
    pub(crate) ops: Vec<TextOp>,
    pub(crate) blocks: Vec<TextBlock>,
    pub(crate) underflows: bool,
    pub(crate) open_states: usize,
}

/// The text-showing operators of `content`, in order, with the text state
/// each runs with. Walks hayro's untyped parser (for where each operator
/// ends) and its typed parser (for the operands, read as the interpreter
/// reads them) in step; both parse the same bytes the same way. Content
/// after an operator the typed parser gives up on is not interpreted by
/// hayro either and is left alone. `None` when an operator's place in the
/// content cannot be told (an operator name with `#` escapes is not kept
/// as a slice of the content).
pub(crate) fn scan(content: &[u8]) -> Option<Scan> {
    let base = content.as_ptr() as usize;
    // Where a slice of the content ends, as an offset.
    let end_of = |slice: &[u8]| -> Option<usize> {
        let start = (slice.as_ptr() as usize).checked_sub(base)?;
        let end = start.checked_add(slice.len())?;
        (end <= content.len()).then_some(end)
    };
    let mut untyped = UntypedIter::new(content);
    let mut typed = TypedIter::new(content);
    let mut params = TextParams::default();
    let mut saved: Vec<TextParams> = Vec::new();
    let mut underflows = false;
    let mut ops: Vec<TextOp> = Vec::new();
    let mut blocks: Vec<TextBlock> = Vec::new();
    // The text object being read.
    let mut block: Option<usize> = None;
    // The last text-showing operator, while the text position it left may
    // still be used.
    let mut open_position: Option<usize> = None;
    let mut previous = 0;
    loop {
        let end = {
            let Some(instruction) = untyped.next() else {
                break;
            };
            let name: &[u8] = instruction.operator;
            let mut end = end_of(name)?;
            if name == b"BI" {
                // The image data follows the operator name, up to "EI".
                let data = instruction.operands().find_map(|o| match o {
                    Object::Stream(s) => Some(s.raw_data()),
                    _ => None,
                })?;
                end = end_of(&data)?;
                if content.get(end..end + 2) != Some(b"EI") {
                    return None;
                }
                end += 2;
            }
            if end < previous {
                return None;
            }
            end
        };
        let Some(op) = typed.next() else {
            break;
        };
        let range = previous..end;
        previous = end;
        if let Some(b) = block
            && matches!(
                op,
                TypedInstruction::SaveState(_)
                    | TypedInstruction::RestoreState(_)
                    | TypedInstruction::Transform(_)
            )
        {
            blocks[b].changes_state = true;
        }
        if matches!(
            op,
            TypedInstruction::BeginText(_)
                | TypedInstruction::EndText(_)
                | TypedInstruction::NextLine(_)
                | TypedInstruction::NextLineAndSetLeading(_)
                | TypedInstruction::NextLineUsingLeading(_)
                | TypedInstruction::SetTextMatrix(_)
                | TypedInstruction::NextLineAndShowText(_)
                | TypedInstruction::ShowTextWithParameters(_)
        ) {
            open_position = None;
        }
        if matches!(
            op,
            TypedInstruction::ShowText(_) | TypedInstruction::ShowTexts(_)
        ) && let Some(p) = open_position
        {
            ops[p].continues = true;
        }
        let (show, items) = match op {
            TypedInstruction::BeginText(_) => {
                // hayro starts a new text object without asking whether the
                // last one ended.
                blocks.push(TextBlock {
                    end: None,
                    changes_state: false,
                    glyphs_before_end: 0,
                });
                block = Some(blocks.len() - 1);
                continue;
            }
            TypedInstruction::EndText(_) => {
                if let Some(b) = block.take() {
                    blocks[b].end = Some(range.end);
                }
                continue;
            }
            TypedInstruction::SaveState(_) => {
                saved.push(params.clone());
                continue;
            }
            TypedInstruction::RestoreState(_) => {
                match saved.pop() {
                    Some(p) => params = p,
                    None => {
                        // hayro returns to the state the content started
                        // with; the editor cannot wrap such content.
                        underflows = true;
                        params = TextParams::default();
                    }
                }
                continue;
            }
            TypedInstruction::TextFont(t) => {
                params.font = Some(t.0.as_ref().to_vec());
                params.size = t.1.as_f64();
                continue;
            }
            TypedInstruction::CharacterSpacing(c) => {
                params.char_space = c.0.as_f64();
                continue;
            }
            TypedInstruction::WordSpacing(w) => {
                params.word_space = w.0.as_f64();
                continue;
            }
            TypedInstruction::ShowText(s) => {
                (Show::Plain, vec![Item::Text(s.0.as_bytes().to_vec())])
            }
            TypedInstruction::NextLineAndShowText(s) => {
                (Show::NextLine, vec![Item::Text(s.0.as_bytes().to_vec())])
            }
            TypedInstruction::ShowTextWithParameters(t) => {
                params.word_space = t.0.as_f64();
                params.char_space = t.1.as_f64();
                let show = Show::NextLineSpaced {
                    word: t.0.as_f64(),
                    char: t.1.as_f64(),
                };
                (show, vec![Item::Text(t.2.as_bytes().to_vec())])
            }
            TypedInstruction::ShowTexts(array) => {
                let items = array
                    .0
                    .iter::<Object<'_>>()
                    .filter_map(|o| match o {
                        Object::Number(n) => Some(Item::Adjust(n.as_f64())),
                        Object::String(s) => Some(Item::Text(s.as_bytes().to_vec())),
                        _ => None,
                    })
                    .collect();
                (Show::Plain, items)
            }
            _ => continue,
        };
        ops.push(TextOp {
            range,
            show,
            items,
            font: params.font.clone(),
            size: params.size,
            char_space: params.char_space,
            word_space: params.word_space,
            glyphs: 0..0,
            block,
            continues: false,
        });
        open_position = Some(ops.len() - 1);
    }
    Some(Scan {
        ops,
        blocks,
        underflows,
        open_states: saved.len(),
    })
}

/// `content` with a marker (`/FastPdfEditOp<n> BMC` ... `EMC`) around
/// text-showing operator `n` and one (`/FastPdfEditEnd<n> BMC EMC`) after
/// the `ET` of text object `n`.
fn instrument(content: &[u8], ops: &[TextOp], blocks: &[TextBlock]) -> Vec<u8> {
    enum Mark {
        Op(usize),
        End(usize),
    }
    let mut marks: Vec<(usize, Mark)> = ops
        .iter()
        .enumerate()
        .map(|(n, op)| (op.range.start, Mark::Op(n)))
        .chain(
            blocks
                .iter()
                .enumerate()
                .filter_map(|(n, b)| Some((b.end?, Mark::End(n)))),
        )
        .collect();
    marks.sort_by_key(|(at, _)| *at);
    let mut out = Vec::with_capacity(content.len() + marks.len() * 32);
    let mut last = 0;
    for (at, mark) in marks {
        out.extend_from_slice(&content[last..at]);
        last = at;
        match mark {
            Mark::Op(n) => {
                let op = &ops[n];
                out.extend_from_slice(b"\n/");
                out.extend_from_slice(MARKER);
                out.extend_from_slice(format!("{n} BMC\n").as_bytes());
                out.extend_from_slice(&content[op.range.clone()]);
                out.extend_from_slice(b"\nEMC\n");
                last = op.range.end;
            }
            Mark::End(n) => {
                out.extend_from_slice(b"\n/");
                out.extend_from_slice(END_MARKER);
                out.extend_from_slice(format!("{n} BMC EMC\n").as_bytes());
            }
        }
    }
    out.extend_from_slice(&content[last..]);
    out
}

/// Records glyph runs and the marker they are drawn in.
struct Recorder {
    /// The text-showing operator being interpreted.
    current: Option<usize>,
    /// Marked content opened inside the current marker.
    nested: usize,
    /// Each operator's glyphs (its first run; a fill-and-stroke operator
    /// draws its glyphs twice).
    runs: Vec<Option<Range<usize>>>,
    /// Glyphs drawn when each text object ended.
    block_ends: Vec<Option<usize>>,
    glyphs: Vec<PlacedGlyph>,
}

fn solid(paint: &Paint<'_>) -> Option<[f32; 3]> {
    match paint {
        Paint::Color(c) => {
            let [r, g, b, _] = c.to_rgba().components();
            Some([r, g, b])
        }
        Paint::Pattern(_) => None,
    }
}

/// How much a transform scales lengths (the square root of its area
/// scale).
pub(crate) fn scale_of(transform: Affine) -> f64 {
    transform.determinant().abs().sqrt()
}

impl<'a> Device<'a> for Recorder {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}

    fn push_clip_path(&mut self, _: &ClipPath) {}

    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}

    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, mode: &DrawMode) {
        let color = solid(&props.paint);
        let stroke_width = |line_width: f32| f64::from(line_width) * scale_of(props.transform);
        if let Some(n) = self.current
            && let Some(Some(range)) = self.runs.get(n)
        {
            // The stroke pass of fill-and-stroke text.
            if let DrawMode::Stroke(stroke) = mode {
                for glyph in &mut self.glyphs[range.clone()] {
                    glyph.look.mode = Mode::FillStroke;
                    glyph.look.stroke = color;
                    glyph.look.stroke_width = stroke_width(stroke.line_width);
                }
            }
            return;
        }
        let look = match mode {
            DrawMode::Fill(_) => Look {
                mode: Mode::Fill,
                fill: color,
                stroke: None,
                stroke_width: 0.0,
            },
            DrawMode::Stroke(stroke) => Look {
                mode: Mode::Stroke,
                fill: None,
                stroke: color,
                stroke_width: stroke_width(stroke.line_width),
            },
            DrawMode::FillAndStroke(_, stroke) => Look {
                mode: Mode::FillStroke,
                fill: color,
                stroke: color,
                stroke_width: stroke_width(stroke.line_width),
            },
            DrawMode::Invisible => Look {
                mode: Mode::Invisible,
                fill: color,
                stroke: None,
                stroke_width: 0.0,
            },
        };
        let start = self.glyphs.len();
        for glyph in run.glyphs() {
            if self.glyphs.len() >= MAX_GLYPHS {
                break;
            }
            let text = match glyph.as_unicode() {
                Some(BfString::Char(c)) => c.to_string(),
                Some(BfString::String(s)) => s,
                None => String::new(),
            };
            let (advance, type3) = match &**glyph {
                Glyph::Outline(o) => (o.advance_width().map(f64::from), false),
                Glyph::Type3(_) => (None, true),
            };
            self.glyphs.push(PlacedGlyph {
                op: self.current,
                text,
                transform: props.transform * glyph.transform(),
                ctm: props.transform,
                advance,
                layer_advance: text::layer_advance(glyph),
                look,
                type3,
            });
        }
        if let Some(n) = self.current
            && let Some(slot) = self.runs.get_mut(n)
        {
            *slot = Some(start..self.glyphs.len());
        }
    }

    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}

    fn pop_clip(&mut self) {}

    fn pop_transparency_group(&mut self) {}

    fn begin_marked_content(&mut self, tag: &[u8], _: Option<i32>) {
        let number = |prefix: &[u8]| -> Option<usize> {
            std::str::from_utf8(tag.strip_prefix(prefix)?)
                .ok()?
                .parse()
                .ok()
        };
        if self.current.is_some() {
            self.nested += 1;
            return;
        }
        if let Some(n) = number(END_MARKER) {
            if let Some(slot) = self.block_ends.get_mut(n) {
                *slot = Some(self.glyphs.len());
            }
            return;
        }
        self.current = number(MARKER);
    }

    fn end_marked_content(&mut self) {
        if self.nested > 0 {
            self.nested -= 1;
        } else {
            self.current = None;
        }
    }
}

/// How a font's strings split into character codes.
pub(crate) enum Encoding {
    /// One byte per code (simple fonts).
    Simple,
    /// Codes of a CID font's CMap.
    Cmap(Box<CMap>),
}

/// The encoding of font resource `name`, read the way hayro reads it.
/// `None` for a font the page does not have (hayro draws such text with a
/// stand-in font) and for vertical writing.
pub(crate) fn encoding(resources: &Resources<'_>, name: &[u8]) -> Option<Encoding> {
    let font = resources.fonts.get::<Dict<'_>>(name)?;
    let subtype = font.get::<Name<'_>>(b"Subtype")?;
    if subtype.as_ref() != b"Type0" {
        return Some(Encoding::Simple);
    }
    let cmap = match font.get::<Object<'_>>(b"Encoding")? {
        Object::Name(name) => match CMapName::from_bytes(name.as_ref()) {
            CMapName::IdentityH => CMap::identity_h(),
            CMapName::IdentityV => return None,
            other => CMap::parse(hayro_cmap::load_embedded(other)?, hayro_cmap::load_embedded)?,
        },
        Object::Stream(stream) => CMap::parse(&stream.decoded().ok()?, hayro_cmap::load_embedded)?,
        _ => return None,
    };
    if cmap.metadata().writing_mode == Some(WritingMode::Vertical) {
        return None;
    }
    Some(Encoding::Cmap(Box::new(cmap)))
}

/// A character code of a string: its bytes, and whether word spacing
/// applies to it (a one-byte code 32).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Code {
    pub(crate) bytes: Range<usize>,
    pub(crate) word_space: bool,
}

/// The character codes of `bytes`, split as hayro splits them: for a CID
/// font, the shortest prefix (up to four bytes) the CMap maps, or one byte
/// (read as code 0) when none does.
pub(crate) fn codes(encoding: &Encoding, bytes: &[u8]) -> Vec<Code> {
    match encoding {
        Encoding::Simple => (0..bytes.len())
            .map(|i| Code {
                bytes: i..i + 1,
                word_space: bytes[i] == 32,
            })
            .collect(),
        Encoding::Cmap(cmap) => {
            let mut out = Vec::new();
            let mut i = 0;
            while i < bytes.len() {
                let mut code = 0u32;
                let mut found = None;
                for n in 0..4.min(bytes.len() - i) {
                    code = (code << 8) | u32::from(bytes[i + n]);
                    if cmap.lookup_cid_code(code, (n + 1) as u8).is_some() {
                        found = Some(n + 1);
                        break;
                    }
                }
                let len = found.unwrap_or(1);
                out.push(Code {
                    bytes: i..i + len,
                    word_space: found == Some(1) && bytes[i] == 32,
                });
                i += len;
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operators_are_found_with_their_text_state() {
        let content = b"q /F1 12 Tf 2 Tc BT 1 0 0 1 72 700 Tm (Hello) Tj [(A) -250 (B)] TJ ET Q \
                        BT 3 Tw (x) ' 1 2 (y) \" ET";
        let Scan {
            ops,
            blocks,
            underflows,
            open_states,
        } = scan(content).unwrap();
        assert!(!underflows);
        assert_eq!(open_states, 0);
        assert_eq!(ops.len(), 4);
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            (ops[0].block, ops[1].block, ops[2].block, ops[3].block),
            (Some(0), Some(0), Some(1), Some(1))
        );
        let end = blocks[0].end.unwrap();
        assert_eq!(&content[end - 2..end], b"ET");
        assert!(!blocks[0].changes_state);
        assert_eq!(&content[ops[0].range.clone()], b" (Hello) Tj");
        assert_eq!(ops[0].items, [Item::Text(b"Hello".to_vec())]);
        assert_eq!(ops[0].font.as_deref(), Some(&b"F1"[..]));
        assert_eq!((ops[0].size, ops[0].char_space), (12.0, 2.0));
        assert_eq!(
            ops[1].items,
            [
                Item::Text(b"A".to_vec()),
                Item::Adjust(-250.0),
                Item::Text(b"B".to_vec())
            ]
        );
        // After Q: hayro's initial text state again.
        assert_eq!(ops[2].font, None);
        assert_eq!((ops[2].size, ops[2].word_space), (1.0, 3.0));
        assert_eq!(ops[2].show, Show::NextLine);
        assert_eq!(
            ops[3].show,
            Show::NextLineSpaced {
                word: 1.0,
                char: 2.0
            }
        );
        assert_eq!((ops[3].word_space, ops[3].char_space), (1.0, 2.0));
        // `(Hello) Tj` goes on into the TJ; the TJ ends its text object; `'`
        // starts a new line.
        assert_eq!(
            ops.iter().map(|o| o.continues).collect::<Vec<_>>(),
            [true, false, false, false]
        );
        let Scan { ops, .. } =
            scan(b"BT (a) Tj 2 Tc /F1 9 Tf (b) Tj 0 -9 Td (c) Tj T* (d) Tj ET").unwrap();
        assert_eq!(
            ops.iter().map(|o| o.continues).collect::<Vec<_>>(),
            [true, false, false, false]
        );
    }

    #[test]
    fn inline_images_end_after_their_data() {
        let content = b"q BI /W 2 /H 1 /BPC 8 /CS /G ID \x00\xff EI Q BT (a) Tj ET";
        let Scan { ops, blocks, .. } = scan(content).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(&content[ops[0].range.clone()], b" (a) Tj");
        let marked = instrument(content, &ops, &blocks);
        let text = String::from_utf8_lossy(&marked);
        assert!(text.contains(
            "EI Q BT\n/FastPdfEditOp0 BMC\n (a) Tj\nEMC\n ET\n/FastPdfEditEnd0 BMC EMC\n"
        ));
        // A text object that changes the CTM, and one never ended.
        let Scan { blocks, .. } = scan(b"BT 2 0 0 2 0 0 cm (a) Tj ET BT (b) Tj").unwrap();
        assert!(blocks[0].changes_state);
        assert_eq!(blocks[1].end, None);
    }

    #[test]
    fn unbalanced_states_are_reported() {
        let Scan {
            underflows,
            open_states,
            ..
        } = scan(b"q q Q").unwrap();
        assert!(!underflows);
        assert_eq!(open_states, 1);
        assert!(scan(b"Q q").unwrap().underflows);
    }

    #[test]
    fn cmap_codes_follow_hayro() {
        let identity = Encoding::Cmap(Box::new(CMap::identity_h()));
        let split: Vec<Range<usize>> = codes(&identity, &[0, 1, 0, 32, 7])
            .into_iter()
            .map(|c| c.bytes)
            .collect();
        // Identity-H maps two-byte codes; a lone trailing byte is one code.
        assert_eq!(split, [0..2, 2..4, 4..5]);
        let simple = codes(&Encoding::Simple, b"a b");
        assert_eq!(simple.len(), 3);
        assert!(simple[1].word_space && !simple[0].word_space);
        let wide = codes(&identity, &[0, 32]);
        assert!(
            !wide[0].word_space,
            "a two-byte code 32 gets no word spacing"
        );
    }
}
