//! Text edits (ADR 0013): replacing a line of text on a page.
//!
//! An edit names a line the way the text layer showed it: a span's box
//! and text. The page's glyphs whose boxes have their centre in that box
//! are the line, and their text must be the span's text (once, or repeated
//! when a producer draws a line twice for a bolder look); otherwise the
//! edit is refused rather than removing other text.
//!
//! The line keeps its glyphs up to the first character the new text
//! changes. The others are taken out of the text-showing operators that
//! drew them: each becomes a `TJ` adjustment as large as its advance, so
//! every other glyph of the page stays exactly where it was. The new rest
//! of the line is drawn with the edit's font where the removed glyphs
//! started, in their size, direction and paint. It goes right after the
//! text object (`BT` ... `ET`) of the line's last kept glyph (or first
//! removed one), inside `q` / `Q` and in that object's CTM, so the content
//! has the line's text in reading order, and later content (stamps,
//! frames) is drawn over it as over the old text. A text object that
//! changes the CTM itself, or never ends, gets the new text after the
//! page's content instead (wrapped in `q` / `Q`).
//!
//! The written page is checked by interpreting it again ([`compare`]): it
//! must draw exactly the glyphs it drew before, minus the removed ones, at
//! the same places, with the new text where it was put.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use fastpdf_engine_api::write::{EditProblem, TextEdit, kept_characters, text_after};
use hayro::hayro_syntax::page::Page;
use hayro::kurbo::{Affine, Point, Rect};

use super::content::{self, Analysis, Encoding, Item, Look, Mode, PlacedGlyph, Show, TextOp};
use super::syntax::real;
use crate::geometry::PageGeom;

/// How far (points) a glyph's centre may lie outside the line's box: the
/// box arrives rounded to `f32`.
const BOX_TOLERANCE: f64 = 0.5;
/// Glyph sizes within this ratio of the line's are on the line.
const SIZE_TOLERANCE: f64 = 0.2;
/// How far (points) a glyph may be from where it is expected after
/// editing.
const POSITION_TOLERANCE: f64 = 0.05;

/// Content ranges and the bytes that replace them.
pub(crate) type Replacements = Vec<(Range<usize>, Vec<u8>)>;

/// How to edit a page: operators to write anew and new text to add.
pub(crate) struct Plan {
    /// Text-showing operators without their removed glyphs.
    pub(crate) replacements: Replacements,
    /// New text, in the order of the edits.
    pub(crate) lines: Vec<NewLine>,
    /// Graphics states the content leaves unrestored.
    pub(crate) open_states: usize,
    /// Glyphs removed (indices in drawing order).
    pub(crate) removed: HashSet<usize>,
}

/// New text for the rest of an edited line.
pub(crate) struct NewLine {
    /// Index of the edit in the page's edits.
    pub(crate) edit: usize,
    pub(crate) text: String,
    /// Glyph space (1000 units per em) to page space for the first glyph:
    /// the size and direction of the glyphs it replaces.
    pub(crate) transform: Affine,
    pub(crate) look: Look,
    /// Where it goes: after the `ET` that ends there, or (`None`) after
    /// the content.
    pub(crate) at: Option<usize>,
    /// The CTM (user space to page space) it is drawn with.
    pub(crate) ctm: Affine,
    /// How many of the page's glyphs (removed ones included) are drawn
    /// before it.
    pub(crate) after_glyphs: usize,
}

/// A glyph a page should draw.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Expected {
    pub(crate) text: String,
    pub(crate) origin: Point,
    /// New text (not a glyph the page had).
    pub(crate) new: bool,
}

/// Plans `edits` on `page`.
pub(crate) fn plan(
    page: &Page<'_>,
    analysis: &Analysis,
    edits: &[TextEdit],
) -> Result<Plan, EditProblem> {
    let mut removed: HashSet<usize> = HashSet::new();
    let mut lines = Vec::new();
    for (index, edit) in edits.iter().enumerate() {
        let line = select(analysis, edit)?;
        if line.iter().any(|g| removed.contains(g)) {
            return Err(EditProblem::NotFound);
        }
        let (keep, rest) = split_line(analysis, &line, edit);
        let start = match keep.last() {
            Some(&last) => analysis.glyphs[last].end(),
            None => analysis.glyphs[line[0]].origin(),
        };
        let reference = rest.first().or(keep.last()).map(|&g| &analysis.glyphs[g]);
        removed.extend(rest.iter().copied());
        // What the new text continues with, after the kept glyphs.
        let text = text_after(&edit.new, non_space_count(&keep, analysis)).to_owned();
        let Some(reference) = reference.filter(|_| !text.is_empty()) else {
            continue;
        };
        let t = reference.transform.as_coeffs();
        // After the text object of the last kept glyph (or the first
        // removed one), drawn in its CTM.
        let anchor = &analysis.glyphs[keep.last().copied().unwrap_or(line[0])];
        let block = anchor
            .op
            .and_then(|op| analysis.ops[op].block)
            .and_then(|b| analysis.blocks.get(b))
            .filter(|b| !b.changes_state);
        let (at, ctm, after_glyphs) = match block.and_then(|b| Some((b.end?, b))) {
            Some((end, b)) => (Some(end), anchor.ctm, b.glyphs_before_end),
            None => {
                // After the content, which must then leave the default
                // state behind.
                if analysis.underflows {
                    return Err(EditProblem::Unsupported);
                }
                let geom = PageGeom::from_page(page);
                (None, geom.user_to_page(), analysis.glyphs.len())
            }
        };
        lines.push(NewLine {
            edit: index,
            text,
            transform: Affine::new([t[0], t[1], t[2], t[3], start.x, start.y]),
            look: reference.look,
            at,
            ctm,
            after_glyphs,
        });
    }
    let replacements = rewrite(page, analysis, &removed)?;
    Ok(Plan {
        replacements,
        lines,
        open_states: analysis.open_states,
        removed,
    })
}

/// The page's content with `plan` carried out: its operators written anew
/// and each line's `drawings` entry after its text object (or after the
/// content, which is then wrapped in `q` / `Q`).
pub(crate) fn compose(content: &[u8], plan: &Plan, drawings: &[Vec<u8>]) -> Vec<u8> {
    let mut pieces: Vec<(Range<usize>, &[u8])> = plan
        .replacements
        .iter()
        .map(|(range, bytes)| (range.clone(), bytes.as_slice()))
        .collect();
    for (line, drawing) in plan.lines.iter().zip(drawings) {
        if let Some(at) = line.at {
            pieces.push((at..at, drawing));
        }
    }
    // Stable: lines at one place keep the order of the edits.
    pieces.sort_by_key(|(range, _)| (range.start, range.end));
    let at_end: Vec<&Vec<u8>> = plan
        .lines
        .iter()
        .zip(drawings)
        .filter(|(line, _)| line.at.is_none())
        .map(|(_, drawing)| drawing)
        .collect();
    let mut out = Vec::with_capacity(content.len() + 256);
    if !at_end.is_empty() {
        out.extend_from_slice(b"q\n");
    }
    let mut last = 0;
    for (range, bytes) in pieces {
        out.extend_from_slice(&content[last..range.start]);
        out.push(b'\n');
        out.extend_from_slice(bytes);
        out.push(b'\n');
        last = range.end.max(range.start);
    }
    out.extend_from_slice(&content[last..]);
    if !at_end.is_empty() {
        out.push(b'\n');
        for _ in 0..=plan.open_states {
            out.extend_from_slice(b"Q\n");
        }
        for drawing in at_end {
            out.extend_from_slice(drawing);
        }
    }
    out
}

/// What the edited page must draw: the glyphs it keeps, in drawing order,
/// with each line's new glyphs (`new_glyphs`, in the order of the plan's
/// lines) where the line was put.
pub(crate) fn expected(
    analysis: &Analysis,
    plan: &Plan,
    new_glyphs: &[Vec<Expected>],
) -> Vec<Expected> {
    let mut out = Vec::with_capacity(analysis.glyphs.len() + 64);
    let lines_at = |i: usize, out: &mut Vec<Expected>| {
        for (line, glyphs) in plan.lines.iter().zip(new_glyphs) {
            if line.after_glyphs == i {
                out.extend(glyphs.iter().cloned());
            }
        }
    };
    for (i, glyph) in analysis.glyphs.iter().enumerate() {
        lines_at(i, &mut out);
        if !plan.removed.contains(&i) {
            out.push(Expected {
                text: glyph.text.clone(),
                origin: glyph.origin(),
                new: false,
            });
        }
    }
    lines_at(analysis.glyphs.len(), &mut out);
    out
}

fn without_spaces(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

fn text_of(analysis: &Analysis, glyphs: &[usize]) -> String {
    glyphs
        .iter()
        .flat_map(|&g| analysis.glyphs[g].text.chars())
        .filter(|c| !c.is_whitespace())
        .collect()
}

/// Whether `found` is `wanted` once or more times over.
fn is_repeated(found: &str, wanted: &str) -> bool {
    !wanted.is_empty()
        && found.len().is_multiple_of(wanted.len())
        && found
            .as_bytes()
            .chunks(wanted.len())
            .all(|c| c == wanted.as_bytes())
}

/// The glyphs of the edit's line, in drawing order.
fn select(analysis: &Analysis, edit: &TextEdit) -> Result<Vec<usize>, EditProblem> {
    let r = edit.rect;
    let area = Rect::new(
        f64::from(r.x0.min(r.x1)),
        f64::from(r.y0.min(r.y1)),
        f64::from(r.x0.max(r.x1)),
        f64::from(r.y0.max(r.y1)),
    )
    .inflate(BOX_TOLERANCE, BOX_TOLERANCE);
    let candidates: Vec<usize> = analysis
        .glyphs
        .iter()
        .enumerate()
        .filter(|(_, g)| area.contains(g.bounds().center()))
        .map(|(i, _)| i)
        .collect();
    let wanted = without_spaces(&edit.old);
    let mut line = candidates.clone();
    if !is_repeated(&text_of(analysis, &line), &wanted) {
        // Smaller or larger glyphs reaching into the box (superscripts,
        // drop caps) are not on the line.
        let mut sizes: Vec<f64> = candidates
            .iter()
            .map(|&g| em(&analysis.glyphs[g]))
            .collect();
        sizes.sort_by(f64::total_cmp);
        let Some(&median) = sizes.get(sizes.len() / 2) else {
            return Err(EditProblem::NotFound);
        };
        line.retain(|&g| (em(&analysis.glyphs[g]) - median).abs() <= SIZE_TOLERANCE * median);
        if !is_repeated(&text_of(analysis, &line), &wanted) {
            return Err(EditProblem::NotFound);
        }
    }
    for &g in &line {
        let glyph = &analysis.glyphs[g];
        if glyph.op.is_none() {
            return Err(EditProblem::InObject);
        }
        if glyph.look.mode == Mode::Invisible {
            return Err(EditProblem::Invisible);
        }
        if glyph.type3 {
            return Err(EditProblem::Unsupported);
        }
    }
    Ok(line)
}

/// A glyph's em size in points.
fn em(glyph: &PlacedGlyph) -> f64 {
    let o = glyph.transform * Point::ZERO;
    (glyph.transform * Point::new(0.0, 1000.0) - o).hypot()
}

/// The glyphs a line keeps (its start, up to the first character the edit
/// changes) and the ones it loses.
fn split_line(analysis: &Analysis, line: &[usize], edit: &TextEdit) -> (Vec<usize>, Vec<usize>) {
    let wanted = without_spaces(&edit.old);
    if text_of(analysis, line) != wanted {
        // A line drawn twice is drawn anew as a whole.
        return (Vec::new(), line.to_vec());
    }
    let shared = kept_characters(&edit.old, &edit.new);
    let mut seen = 0;
    let mut keep = 0;
    for &g in line {
        let n = analysis.glyphs[g]
            .text
            .chars()
            .filter(|c| !c.is_whitespace())
            .count();
        // A glyph without text stays when kept glyphs follow it.
        if seen + n <= shared && (n > 0 || seen < shared) {
            seen += n;
            keep += 1;
        } else {
            break;
        }
    }
    (line[..keep].to_vec(), line[keep..].to_vec())
}

fn non_space_count(glyphs: &[usize], analysis: &Analysis) -> usize {
    text_of(analysis, glyphs).chars().count()
}

/// Every text-showing operator that drew a removed glyph, written again
/// without it: its content range and new bytes.
fn rewrite(
    page: &Page<'_>,
    analysis: &Analysis,
    removed: &HashSet<usize>,
) -> Result<Replacements, EditProblem> {
    let mut encodings: HashMap<Vec<u8>, Option<Encoding>> = HashMap::new();
    let mut out = Vec::new();
    for op in &analysis.ops {
        if !op.glyphs.clone().any(|g| removed.contains(&g)) {
            continue;
        }
        let font = op.font.as_ref().ok_or(EditProblem::Unsupported)?;
        let encoding = encodings
            .entry(font.clone())
            .or_insert_with(|| content::encoding(page.resources(), font))
            .as_ref()
            .ok_or(EditProblem::Unsupported)?;
        let written = rewrite_op(op, &analysis.glyphs, removed, encoding)?;
        out.push((op.range.clone(), written));
    }
    Ok(out)
}

/// A string or adjustment of a `TJ` array being written.
enum Element {
    Text(Vec<u8>),
    Adjust(f64),
}

/// `op` as a `TJ` operator without the removed glyphs: each becomes an
/// adjustment of its advance (glyph width, character spacing and, for a
/// one-byte code 32, word spacing), so the operator moves the text
/// position exactly as far as before.
fn rewrite_op(
    op: &TextOp,
    glyphs: &[PlacedGlyph],
    removed: &HashSet<usize>,
    encoding: &Encoding,
) -> Result<Vec<u8>, EditProblem> {
    if op.size == 0.0 || !op.size.is_finite() {
        return Err(EditProblem::Unsupported);
    }
    let mut elements: Vec<Element> = Vec::new();
    let adjust = |elements: &mut Vec<Element>, amount: f64| match elements.last_mut() {
        Some(Element::Adjust(a)) => *a += amount,
        _ => elements.push(Element::Adjust(amount)),
    };
    let mut index = op.glyphs.start;
    for item in &op.items {
        match item {
            Item::Adjust(amount) => adjust(&mut elements, *amount),
            Item::Text(bytes) => {
                for code in content::codes(encoding, bytes) {
                    if index >= op.glyphs.end {
                        return Err(EditProblem::Unsupported);
                    }
                    if removed.contains(&index) {
                        let width = glyphs[index].advance.unwrap_or(0.0);
                        let word = if code.word_space { op.word_space } else { 0.0 };
                        let spacing = (op.char_space + word) * 1000.0 / op.size;
                        adjust(&mut elements, -(width + spacing));
                    } else {
                        match elements.last_mut() {
                            Some(Element::Text(text)) => text.extend_from_slice(&bytes[code.bytes]),
                            _ => elements.push(Element::Text(bytes[code.bytes].to_vec())),
                        }
                    }
                    index += 1;
                }
            }
        }
    }
    if index != op.glyphs.end {
        return Err(EditProblem::Unsupported);
    }
    if !op.continues {
        // Nothing goes on from where the operator leaves the text position:
        // its trailing adjustments move nothing.
        while matches!(elements.last(), Some(Element::Adjust(_))) {
            elements.pop();
        }
        if elements.is_empty() && op.show == Show::Plain {
            return Ok(Vec::new());
        }
    }
    let mut out = match op.show {
        Show::Plain => Vec::new(),
        Show::NextLine => b"T* ".to_vec(),
        Show::NextLineSpaced { word, char } => {
            format!("{} Tw {} Tc T* ", real(word), real(char)).into_bytes()
        }
    };
    out.push(b'[');
    for element in elements {
        match element {
            Element::Text(bytes) => {
                out.push(b'<');
                for b in bytes {
                    out.extend_from_slice(format!("{b:02X}").as_bytes());
                }
                out.push(b'>');
            }
            Element::Adjust(amount) if amount != 0.0 => {
                out.push(b' ');
                out.extend_from_slice(real(amount).as_bytes());
                out.push(b' ');
            }
            Element::Adjust(_) => {}
        }
    }
    out.extend_from_slice(b"] TJ");
    Ok(out)
}

/// Where a check of an edited page failed.
#[derive(Debug, PartialEq)]
pub(crate) enum Mismatch {
    /// A glyph that was to stay is missing or moved.
    Kept(String),
    /// The new text is missing, moved or reads wrong.
    New(String),
}

/// Checks that `glyphs` (what an edited page draws) are `expected`.
pub(crate) fn compare(glyphs: &[PlacedGlyph], expected: &[Expected]) -> Result<(), Mismatch> {
    let fail = |i: usize, what: String| {
        if expected.get(i).is_some_and(|e| e.new) {
            Mismatch::New(what)
        } else {
            Mismatch::Kept(what)
        }
    };
    for (i, want) in expected.iter().enumerate() {
        let Some(glyph) = glyphs.get(i) else {
            return Err(fail(i, format!("glyph {i} ({:?}) is missing", want.text)));
        };
        if glyph.text != want.text {
            return Err(fail(
                i,
                format!(
                    "glyph {i} reads {:?} instead of {:?}",
                    glyph.text, want.text
                ),
            ));
        }
        let distance = (glyph.origin() - want.origin).hypot();
        if distance > POSITION_TOLERANCE {
            return Err(fail(
                i,
                format!("glyph {i} ({:?}) moved {distance:.3} pt", want.text),
            ));
        }
    }
    if glyphs.len() > expected.len() {
        return Err(Mismatch::New(format!(
            "{} glyphs instead of {}",
            glyphs.len(),
            expected.len()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_lines_are_recognized() {
        assert!(is_repeated("abab", "ab"));
        assert!(is_repeated("ab", "ab"));
        assert!(!is_repeated("aba", "ab"));
        assert!(!is_repeated("", ""));
    }
}
