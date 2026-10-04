//! Reading order: the characters of a text layer the way the page lays
//! them out. Copy (`fastpdf-core`'s `selection::copy_text`) writes this
//! text, and search (`fastpdf-search`) matches against it, so a phrase that
//! copies as one line or across a line break is found the same way.
//!
//! [`TextLayer::lay_out`] takes a range of characters in content order and
//! only decides their order and the whitespace between them:
//!
//! 1. **Direction.** Each span's glyphs advance right, down, left or up in
//!    page space. Its text is read in that direction's frame: `u` along the
//!    text, `v` toward the next line (down for horizontal text, leftward for
//!    vertical CJK). Rotated pages need no special case: a page with
//!    `/Rotate` whose text reads upright on screen draws that text rotated
//!    in page space, and that is the direction its glyphs advance in.
//! 2. **Lines.** The characters of one span in the range (a *fragment*)
//!    join a line when they sit on the line's baseline and either directly
//!    follow one of the line's fragments in content order, at any distance
//!    (table cells, tab stops), or lie within a character height of one (a
//!    line drawn in pieces, out of order). Column gutters are wider, so
//!    columns stay apart.
//! 3. **Order.** Fragments of a line by `u`. Lines in content order, except
//!    that a line never comes before a line above it in the same column: the
//!    lines of a column read top to bottom, while columns and blocks side by
//!    side keep the order in which the document draws them. A page's running
//!    header and footer (text set apart at its very top or bottom) read
//!    first and last, wherever the document draws them.
//! 4. **Whitespace.** A space between neighbours on a line that are visibly
//!    apart, unless the text already has whitespace there. Two CJK
//!    characters, written without word spaces, are only separated by a blank
//!    at least a character wide (table cells): narrower gaps get no space,
//!    and plain spaces in them are dropped. A line break between lines; a
//!    paragraph break where the lines of a column are clearly further apart
//!    than usual. Hyphenated words are not joined: telling a line-end hyphen
//!    from a real one is too unreliable.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::ops::Range;

use super::{TextLayer, TextSpan};
use crate::PageRect;

/// A gap wider than this many character heights (about the font size)
/// between neighbours on a line is a word space. Letter spacing stays below
/// it; a word space is about 0.25 em.
const SPACE_GAP: f32 = 0.2;
/// Between two CJK characters only a blank at least one character wide
/// separates anything (table cells, tab stops). Narrower gaps come from
/// justification, letter spacing or half-width punctuation: they get no
/// space, and spaces an engine put into them are dropped.
const CJK_GAP: f32 = 1.0;
/// Fragments on one baseline overlap vertically by at least this fraction
/// of the smaller one's height (superscripts and subscripts still do).
const SAME_LINE_OVERLAP: f32 = 0.5;
/// How far (in character heights) a fragment may start before the end of
/// the previous one in content order and still continue its line (kerning,
/// italic overhang).
const BACKTRACK: f32 = 0.5;
/// Largest gap, in character heights, between a fragment of a line and a
/// fragment that joins it out of content order. Column gutters are wider.
const JOIN_GAP: f32 = 1.0;
/// Out-of-order fragments only join fragments of similar height, so that a
/// drop cap does not pull in the lines beside it.
const JOIN_HEIGHT_RATIO: f32 = 2.0;
/// Inside a span, a character this many heights off the previous one's
/// centre line, or this far behind it, starts a new fragment.
const SPLIT_ACROSS: f32 = 0.6;
const SPLIT_BACK: f32 = 1.0;
/// Lines share a column when they overlap along the text by this fraction
/// of the narrower one.
const COLUMN_OVERLAP: f32 = 0.5;
/// The boxes of a line and the line below it may overlap by this fraction
/// of a line height (tight leading).
const ABOVE_TOLERANCE: f32 = 0.3;
/// Consecutive lines of a column form a block, in which paragraph breaks
/// are judged, when their heights are within this ratio...
const BLOCK_HEIGHT_RATIO: f32 = 1.5;
/// ...and their baselines at most this many character sizes apart. A
/// larger step down the same column is a paragraph break by itself.
const BLOCK_MAX_SPACING: f32 = 3.0;
/// Line pairs whose heights are within this ratio set a block's usual line
/// spacing...
const BODY_HEIGHT_RATIO: f32 = 1.15;
/// ...which is only trusted with this many of them.
const MIN_BODY_PAIRS: usize = 3;
/// A paragraph break: baselines this many times the usual spacing apart.
const PARAGRAPH_SPACING: f32 = 1.35;
/// Ordering compares every pair of lines; with more lines than this (only
/// pathological pages), lines stay in content order.
const MAX_ORDERED_LINES: usize = 4000;
/// A running header or footer: at most this many spans level with each
/// other at the top or bottom of the page...
const MAX_FURNITURE_SPANS: usize = 4;
/// ...above (below) at least this many other spans...
const MIN_BODY_SPANS: usize = 2;
/// ...and set apart from them by this many of its heights.
const FURNITURE_GAP: f32 = 1.0;
/// Cell size (points along `v`) of the index of fragments by position.
const CELL: f32 = 4.0;
/// Bounds on one lookup in that index (huge text, crowded pages).
const MAX_CELLS: i64 = 64;
const MAX_CANDIDATES: usize = 512;
/// Smallest size used in relative tolerances (degenerate boxes).
const MIN_SIZE: f32 = 0.01;

/// A `char` of a text layer: the span, and the `char` index in its text.
/// Ranges of them are in content order (span by span).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct CharPos {
    pub span: usize,
    pub ch: usize,
}

impl CharPos {
    /// Before the first `char` of every layer.
    pub const START: Self = Self { span: 0, ch: 0 };
    /// After the last `char` of every layer: `START..END` is the whole page.
    pub const END: Self = Self {
        span: usize::MAX,
        ch: usize::MAX,
    };
}

/// A piece of laid-out text, as [`TextLayer::lay_out`] hands it over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaidText {
    /// A character of the layer. Whitespace characters only come where the
    /// text keeps them: indentation at the start of a line, and the
    /// document's own spaces between words.
    Char { ch: char, at: CharPos },
    /// A space between neighbours drawn apart without one.
    Space,
    /// The end of a line; the next line follows.
    Break(LineBreak),
}

/// How one line ends before the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineBreak {
    Line,
    /// The lines are clearly further apart than the lines around them, or
    /// a heading of another size stands above the next line.
    Paragraph,
}

impl TextLayer {
    /// Lays out the characters from `range.start` (inclusive) to
    /// `range.end` (exclusive), in content order, the way the page shows
    /// them (see the module documentation), and hands the pieces to `emit`
    /// in reading order: each line's characters and the spaces between its
    /// words, with a [`LaidText::Break`] between lines. Trailing whitespace
    /// of a line is dropped. `CharPos::START..CharPos::END` lays out the
    /// whole page.
    ///
    /// Works on hostile geometry (non-finite or huge boxes) and stays fast
    /// on crowded pages: lookups are bounded, and pages with thousands of
    /// lines keep their lines in content order.
    pub fn lay_out(&self, range: Range<CharPos>, emit: impl FnMut(LaidText)) {
        lay_out(self, range, true, emit);
    }

    /// [`TextLayer::lay_out`] without telling paragraphs apart: every break
    /// between lines is a [`LineBreak::Line`]. The lines, their order and
    /// the spaces are the same; measuring line spacing is skipped (search
    /// only needs to know where lines end).
    pub fn lay_out_lines(&self, range: Range<CharPos>, emit: impl FnMut(LaidText)) {
        lay_out(self, range, false, emit);
    }
}

fn lay_out(
    layer: &TextLayer,
    range: Range<CharPos>,
    paragraphs: bool,
    mut emit: impl FnMut(LaidText),
) {
    let dirs = span_dirs(layer);
    let (glyphs, frags) = collect(layer, range.start, range.end, &dirs);
    let lines = build_lines(&frags);
    let ordered: Vec<&Line> = order_lines(&lines, &furniture(layer, &dirs))
        .into_iter()
        .filter_map(|i| lines.get(i))
        .collect();
    let breaks = if paragraphs {
        let mut scratch = [Vec::new(), Vec::new(), Vec::new()];
        let shapes: Vec<Shape> = ordered
            .iter()
            .map(|line| shape(line, &frags, &glyphs, &mut scratch))
            .collect();
        line_breaks(&shapes)
    } else {
        vec![LineBreak::Line; ordered.len().saturating_sub(1)]
    };
    for (i, line) in ordered.iter().enumerate() {
        if let Some(&b) = i.checked_sub(1).and_then(|i| breaks.get(i)) {
            emit(LaidText::Break(b));
        }
        emit_line(line, &frags, &glyphs, &mut emit);
    }
}

/// Direction in which a span's glyphs advance, in page space (y down).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Dir {
    Right,
    Down,
    Left,
    Up,
}

impl Dir {
    const ALL: [Dir; 4] = [Dir::Right, Dir::Down, Dir::Left, Dir::Up];

    /// `r` in this direction's frame: `u` along the text, `v` toward the
    /// next line (a quarter turn clockwise from `u`).
    fn frame(self, r: PageRect) -> Bx {
        let r = finite(r);
        match self {
            Dir::Right => Bx::new(r.x0, r.x1, r.y0, r.y1),
            Dir::Down => Bx::new(r.y0, r.y1, -r.x1, -r.x0),
            Dir::Left => Bx::new(-r.x1, -r.x0, -r.y1, -r.y0),
            Dir::Up => Bx::new(-r.y1, -r.y0, r.x0, r.x1),
        }
    }
}

/// A box in a direction's frame.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Bx {
    u0: f32,
    u1: f32,
    v0: f32,
    v1: f32,
}

impl Bx {
    fn new(u0: f32, u1: f32, v0: f32, v1: f32) -> Self {
        Self {
            u0: u0.min(u1),
            u1: u0.max(u1),
            v0: v0.min(v1),
            v1: v0.max(v1),
        }
    }

    fn width(self) -> f32 {
        self.u1 - self.u0
    }

    fn height(self) -> f32 {
        self.v1 - self.v0
    }

    fn v_mid(self) -> f32 {
        (self.v0 + self.v1) / 2.0
    }

    fn union(self, other: Self) -> Self {
        Self {
            u0: self.u0.min(other.u0),
            u1: self.u1.max(other.u1),
            v0: self.v0.min(other.v0),
            v1: self.v1.max(other.v1),
        }
    }

    /// Overlap along the text; negative: the gap between them.
    fn u_overlap(self, other: Self) -> f32 {
        self.u1.min(other.u1) - self.u0.max(other.u0)
    }

    /// Overlap across the text; negative: the gap between them.
    fn v_overlap(self, other: Self) -> f32 {
        self.v1.min(other.v1) - self.v0.max(other.v0)
    }
}

/// `r` with non-finite coordinates (hostile files) replaced by zero.
fn finite(r: PageRect) -> PageRect {
    let f = |v: f32| if v.is_finite() { v } else { 0.0 };
    PageRect::new(f(r.x0), f(r.y0), f(r.x1), f(r.y1))
}

/// The direction of every span. Spans that do not show one (a single
/// character) take the direction most of the page's text has.
fn span_dirs(layer: &TextLayer) -> Vec<Dir> {
    let mut weight = [0usize; 4];
    let own: Vec<Option<Dir>> = layer
        .spans
        .iter()
        .map(|span| {
            let n = span.char_count();
            let dir = span_dir(span, n);
            if let Some(dir) = dir {
                weight[dir as usize] += n;
            }
            dir
        })
        .collect();
    // Ties go to the earlier direction: plain pages read left to right.
    let dominant = Dir::ALL
        .into_iter()
        .zip(weight)
        .fold(
            (Dir::Right, 0),
            |best, (dir, w)| {
                if w > best.1 { (dir, w) } else { best }
            },
        )
        .0;
    own.into_iter().map(|d| d.unwrap_or(dominant)).collect()
}

/// A span's direction (`n`: its `char` count): from the centre of its first
/// non-blank character to that of its last, when they are clearly apart.
fn span_dir(span: &TextSpan, n: usize) -> Option<Dir> {
    let first = span.text.chars().position(|c| !c.is_whitespace())?;
    let from_end = span.text.chars().rev().position(|c| !c.is_whitespace())?;
    let last = n.checked_sub(from_end + 1)?;
    let (a, b) = (finite(span.char_rect(first)), finite(span.char_rect(last)));
    let dx = (b.x0 + b.x1 - a.x0 - a.x1) / 2.0;
    let dy = (b.y0 + b.y1 - a.y0 - a.y1) / 2.0;
    let size = a.width().max(a.height()).max(b.width()).max(b.height());
    if dx.abs().max(dy.abs()) <= 0.25 * size {
        return None;
    }
    Some(if dx.abs() >= dy.abs() {
        if dx > 0.0 { Dir::Right } else { Dir::Left }
    } else if dy > 0.0 {
        Dir::Down
    } else {
        Dir::Up
    })
}

/// A character in the range and its box in its span's frame.
#[derive(Debug)]
struct Glyph {
    ch: char,
    b: Bx,
}

/// The characters in the range of one span on one line.
#[derive(Debug)]
struct Fragment {
    dir: Dir,
    glyphs: Range<usize>,
    /// Where its first glyph comes from; the others follow in the span.
    at: CharPos,
    /// Union of the boxes of its non-blank characters.
    ink: Bx,
}

/// The characters in the range in content order, cut into fragments. Runs
/// of whitespace alone make no fragment: gaps stand for them.
fn collect(
    layer: &TextLayer,
    start: CharPos,
    end: CharPos,
    dirs: &[Dir],
) -> (Vec<Glyph>, Vec<Fragment>) {
    let mut glyphs = Vec::new();
    let mut frags = Vec::new();
    let Some(last) = layer.spans.len().checked_sub(1) else {
        return (glyphs, frags);
    };
    // Room for every char of the spans in the range, so that laying out a
    // whole page does not regrow the buffer over and over.
    glyphs.reserve(
        layer
            .spans
            .get(start.span..=end.span.min(last))
            .map_or(0, |spans| spans.iter().map(TextSpan::char_count).sum()),
    );
    for si in start.span..=end.span.min(last) {
        let (Some(span), Some(&dir)) = (layer.spans.get(si), dirs.get(si)) else {
            break;
        };
        let n = span.char_count();
        let from = if si == start.span { start.ch } else { 0 };
        let to = if si == end.span { end.ch.min(n) } else { n };
        if from >= to {
            continue;
        }
        // Glyph `k` of this span is its `char` `from + k - first`.
        let first = glyphs.len();
        let at = |begin: usize| CharPos {
            span: si,
            ch: from + (begin - first),
        };
        let mut begin = glyphs.len();
        let mut ink: Option<Bx> = None;
        let mut last_ink: Option<Bx> = None;
        for (ch, r) in span
            .text
            .chars()
            .zip(span.char_rects())
            .skip(from)
            .take(to - from)
        {
            let b = dir.frame(r);
            if !ch.is_whitespace() {
                if let Some(prev) = last_ink
                    && jumps(prev, b)
                {
                    push_fragment(&mut frags, dir, begin..glyphs.len(), at(begin), ink.take());
                    begin = glyphs.len();
                }
                ink = Some(ink.map_or(b, |i| i.union(b)));
                last_ink = Some(b);
            }
            glyphs.push(Glyph { ch, b });
        }
        push_fragment(&mut frags, dir, begin..glyphs.len(), at(begin), ink);
    }
    (glyphs, frags)
}

fn push_fragment(
    frags: &mut Vec<Fragment>,
    dir: Dir,
    glyphs: Range<usize>,
    at: CharPos,
    ink: Option<Bx>,
) {
    if let Some(ink) = ink {
        frags.push(Fragment {
            dir,
            glyphs,
            at,
            ink,
        });
    }
}

/// Whether `b` leaves the line of `prev` inside one span (engines keep a
/// span on one line; this only guards against those that do not).
fn jumps(prev: Bx, b: Bx) -> bool {
    let h = prev.height().max(b.height()).max(MIN_SIZE);
    (b.v_mid() - prev.v_mid()).abs() > SPLIT_ACROSS * h || b.u0 < prev.u1 - SPLIT_BACK * h
}

/// A line: fragments sharing a baseline.
#[derive(Debug)]
struct Line {
    dir: Dir,
    /// Its fragments along the text.
    frags: Vec<usize>,
    ink: Bx,
}

/// Groups fragments into lines. Lines come out in content order (of their
/// first fragment).
fn build_lines(frags: &[Fragment]) -> Vec<Line> {
    let mut sets = DisjointSets::new(frags.len());
    // The box of each line so far, kept at its root.
    let mut ink: Vec<Bx> = frags.iter().map(|f| f.ink).collect();
    // Fragments by direction and the cell of their centre along `v`.
    let mut index: HashMap<(Dir, i64), Vec<usize>> = HashMap::new();
    for (i, f) in frags.iter().enumerate() {
        if let Some(prev) = i.checked_sub(1).and_then(|p| frags.get(p)) {
            let line = sets.find(i - 1);
            if continues(prev, ink.get(line).copied().unwrap_or(prev.ink), f) {
                merge(&mut sets, &mut ink, i - 1, i);
            }
        }
        for j in nearby(&index, f) {
            if frags.get(j).is_some_and(|other| joins(other, f)) {
                merge(&mut sets, &mut ink, j, i);
            }
        }
        index
            .entry((f.dir, cell(f.ink.v_mid())))
            .or_default()
            .push(i);
    }
    let mut line_of_root: Vec<Option<usize>> = vec![None; frags.len()];
    let mut lines: Vec<Line> = Vec::new();
    for (i, f) in frags.iter().enumerate() {
        let root = sets.find(i);
        match line_of_root.get(root).copied().flatten() {
            Some(l) => {
                if let Some(line) = lines.get_mut(l) {
                    line.frags.push(i);
                    line.ink = line.ink.union(f.ink);
                }
            }
            None => {
                if let Some(slot) = line_of_root.get_mut(root) {
                    *slot = Some(lines.len());
                }
                lines.push(Line {
                    dir: f.dir,
                    frags: vec![i],
                    ink: f.ink,
                });
            }
        }
    }
    for line in &mut lines {
        // Stable: fragments at the same position keep content order.
        line.frags.sort_by(|&a, &b| {
            let (a, b) = (frags.get(a), frags.get(b));
            let u = |f: Option<&Fragment>| f.map_or(0.0, |f| f.ink.u0);
            u(a).total_cmp(&u(b))
        });
    }
    lines
}

fn cell(v: f32) -> i64 {
    (v / CELL).floor() as i64
}

/// Earlier fragments whose centre is within `f`'s height of `f`'s centre:
/// every fragment that `joins` can accept, since those are at most twice
/// as high as `f` and overlap its centre line.
fn nearby<'a>(
    index: &'a HashMap<(Dir, i64), Vec<usize>>,
    f: &Fragment,
) -> impl Iterator<Item = usize> + 'a {
    let reach = f.ink.height().max(MIN_SIZE);
    let (lo, hi) = (cell(f.ink.v_mid() - reach), cell(f.ink.v_mid() + reach));
    // Text this large is rare; it only loses out-of-order joins.
    let cells = (hi.saturating_sub(lo) <= MAX_CELLS).then_some(lo..=hi);
    let dir = f.dir;
    cells
        .into_iter()
        .flatten()
        .filter_map(move |c| index.get(&(dir, c)))
        .flatten()
        .copied()
        .take(MAX_CANDIDATES)
}

fn same_baseline(a: Bx, b: Bx) -> bool {
    a.v_overlap(b) >= SAME_LINE_OVERLAP * a.height().min(b.height())
}

/// `f` continues the line (box `line`) of `prev`, the fragment just before
/// it in content order: it lies across that line, and does not go back.
/// The whole line counts, so that a subscript right after a superscript
/// stays on it.
fn continues(prev: &Fragment, line: Bx, f: &Fragment) -> bool {
    let (a, b) = (prev.ink.height(), f.ink.height());
    prev.dir == f.dir
        && line.v_overlap(f.ink) >= SAME_LINE_OVERLAP * a.min(b)
        && f.ink.u0 >= prev.ink.u1 - BACKTRACK * a.max(b)
}

/// Puts the fragments `a` and `b` on one line.
fn merge(sets: &mut DisjointSets, ink: &mut [Bx], a: usize, b: usize) {
    let (ra, rb) = (sets.find(a), sets.find(b));
    if ra == rb {
        return;
    }
    let both = match (ink.get(ra), ink.get(rb)) {
        (Some(x), Some(y)) => x.union(*y),
        _ => return,
    };
    let root = sets.union(ra, rb);
    if let Some(slot) = ink.get_mut(root) {
        *slot = both;
    }
}

/// `f` belongs on the line of `other`, an earlier fragment that is not
/// just before it in content order: same baseline, similar size, close by.
fn joins(other: &Fragment, f: &Fragment) -> bool {
    let (a, b) = (other.ink.height(), f.ink.height());
    let (lo, hi) = (a.min(b), a.max(b));
    other.dir == f.dir
        && hi <= JOIN_HEIGHT_RATIO * lo
        && same_baseline(other.ink, f.ink)
        && -other.ink.u_overlap(f.ink) <= JOIN_GAP * lo
}

/// Union–find over fragment indices; a set's root is its smallest index.
#[derive(Debug)]
struct DisjointSets {
    parent: Vec<usize>,
}

impl DisjointSets {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
        }
    }

    fn find(&mut self, mut i: usize) -> usize {
        while let Some(&p) = self.parent.get(i) {
            if p == i {
                break;
            }
            // Path halving.
            let grand = self.parent.get(p).copied().unwrap_or(p);
            if let Some(slot) = self.parent.get_mut(i) {
                *slot = grand;
            }
            i = grand;
        }
        i
    }

    /// Joins the sets of `a` and `b`; returns the root of the result.
    fn union(&mut self, a: usize, b: usize) -> usize {
        let (ra, rb) = (self.find(a), self.find(b));
        let root = ra.min(rb);
        if ra != rb
            && let Some(slot) = self.parent.get_mut(ra.max(rb))
        {
            *slot = root;
        }
        root
    }
}

/// Reading order of `lines` (indices): content order, except that a line
/// waits for every line above it in its column, and running headers and
/// footers come before and after everything else.
fn order_lines(lines: &[Line], furniture: &[Furniture; 4]) -> Vec<usize> {
    let n = lines.len();
    if n > MAX_ORDERED_LINES {
        return (0..n).collect();
    }
    // What the order depends on, line by line, packed for the pairwise pass.
    let keys: Vec<(Dir, Bx, Role)> = lines
        .iter()
        .zip(roles(lines, furniture))
        .map(|(line, role)| (line.dir, line.ink, role))
        .collect();
    // For every line, how many lines must come before it, and (row `y` of a
    // bit matrix, at most 2 MB) the lines it must come before.
    let words = n.div_ceil(64);
    let mut waiting: Vec<usize> = vec![0; n];
    let mut before = vec![0u64; n * words];
    for (row, &y) in before.chunks_mut(words.max(1)).zip(&keys) {
        for (x, (w, &line)) in waiting.iter_mut().zip(&keys).enumerate() {
            if precedes(y, line) {
                *w += 1;
                if let Some(bits) = row.get_mut(x / 64) {
                    *bits |= 1 << (x % 64);
                }
            }
        }
    }
    // Lines are in content order, so the smallest ready index comes next.
    let mut ready: BinaryHeap<Reverse<usize>> = waiting
        .iter()
        .enumerate()
        .filter(|(_, w)| **w == 0)
        .map(|(i, _)| Reverse(i))
        .collect();
    let mut placed = vec![false; n];
    let mut order = Vec::with_capacity(n);
    while let Some(Reverse(y)) = ready.pop() {
        order.push(y);
        if let Some(p) = placed.get_mut(y) {
            *p = true;
        }
        let row = before.get(y * words..(y + 1) * words).unwrap_or(&[]);
        for (i, &bits) in row.iter().enumerate() {
            // Set bits in increasing order: the lines `y` comes before.
            let mut bits = bits;
            while bits != 0 {
                let x = i * 64 + bits.trailing_zeros() as usize;
                bits &= bits - 1;
                if !placed.get(x).copied().unwrap_or(true)
                    && let Some(w) = waiting.get_mut(x)
                {
                    *w = w.saturating_sub(1);
                    if *w == 0 {
                        ready.push(Reverse(x));
                    }
                }
            }
        }
    }
    // Every rule above orders lines by their position along `v`, so there
    // are no cycles; this only keeps every line should that ever change.
    if order.len() < n {
        order.extend((0..n).filter(|&i| !placed.get(i).copied().unwrap_or(true)));
    }
    order
}

/// Whether line `y` must be read before line `x` (direction, box, role
/// each): it lies above `x` in the same column, or it is part of the
/// running header and `x` is not, or `x` is part of the footer and `y` is
/// not. Lines of different directions are not ordered.
fn precedes(
    (y_dir, y_ink, y_role): (Dir, Bx, Role),
    (x_dir, x_ink, x_role): (Dir, Bx, Role),
) -> bool {
    y_dir == x_dir
        && (above_in_column(y_ink, x_ink)
            || (y_role == Role::Header && x_role != Role::Header)
            || (x_role == Role::Footer && y_role != Role::Footer))
}

/// Whether box `a` lies above box `b` in the same column (overlapping
/// along the text), so that it must be read first.
fn above_in_column(a: Bx, b: Bx) -> bool {
    a.v_mid() < b.v_mid()
        && a.v1 <= b.v0 + ABOVE_TOLERANCE * a.height().min(b.height())
        && a.u_overlap(b) >= COLUMN_OVERLAP * a.width().min(b.width())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Body,
    Header,
    Footer,
}

/// Where a page's running header ends and its footer starts, along `v` of
/// one direction: the text at the very top (bottom) of the page, set apart
/// from all the rest by a clear gap. Headers and footers often sit outside
/// every column (a page number centred under two columns), and documents
/// draw them first or last. Found on the whole page, so that a range that
/// merely starts or ends with a heading is not affected.
#[derive(Debug, Clone, Copy, Default)]
struct Furniture {
    header_below: Option<f32>,
    footer_above: Option<f32>,
}

fn furniture(layer: &TextLayer, dirs: &[Dir]) -> [Furniture; 4] {
    let mut out = [Furniture::default(); 4];
    for dir in Dir::ALL {
        let boxes: Vec<Bx> = layer
            .spans
            .iter()
            .zip(dirs)
            .filter(|(span, d)| **d == dir && !span.text.trim().is_empty())
            .map(|(span, _)| dir.frame(span.bounds))
            .collect();
        let found = &mut out[dir as usize];
        found.footer_above = edge_band(&boxes, 1.0);
        found.header_below = edge_band(&boxes, -1.0).map(|v| -v);
    }
    out
}

/// The top (along `v` times `flip`) of the text level with the lowest box,
/// when at most a few boxes are level with it and all others end clearly
/// above it.
fn edge_band(boxes: &[Bx], flip: f32) -> Option<f32> {
    let along = |b: &Bx| Bx::new(b.u0, b.u1, flip * b.v0, flip * b.v1);
    let edge = boxes
        .iter()
        .map(along)
        .max_by(|a, b| a.v0.total_cmp(&b.v0))?;
    let (group, rest): (Vec<Bx>, Vec<Bx>) = boxes
        .iter()
        .map(along)
        .partition(|b| b.v_overlap(edge) > 0.0);
    let top = group.iter().map(|b| b.v0).fold(f32::INFINITY, f32::min);
    let height = group.iter().map(|b| b.height()).fold(0.0, f32::max);
    let rest_bottom = rest.iter().map(|b| b.v1).fold(f32::NEG_INFINITY, f32::max);
    (group.len() <= MAX_FURNITURE_SPANS
        && rest.len() >= MIN_BODY_SPANS
        && top - rest_bottom >= FURNITURE_GAP * height)
        .then_some(top)
}

fn roles(lines: &[Line], furniture: &[Furniture; 4]) -> Vec<Role> {
    lines
        .iter()
        .map(|line| {
            let f = furniture[line.dir as usize];
            let slack = MIN_SIZE.max(0.01 * line.ink.height());
            if f.footer_above.is_some_and(|v| line.ink.v0 >= v - slack) {
                Role::Footer
            } else if f.header_below.is_some_and(|v| line.ink.v1 <= v + slack) {
                Role::Header
            } else {
                Role::Body
            }
        })
        .collect()
}

/// What placing breaks around a line needs to know about it.
#[derive(Debug)]
struct Shape {
    dir: Dir,
    ink: Bx,
    /// Median bottom of its characters (standing in for the baseline) and
    /// median character height.
    base: f32,
    height: f32,
    /// Median of the characters' larger side, a stand-in for the font size:
    /// engines may report vertical text's boxes only half an em across.
    size: f32,
}

/// `scratch`: buffers reused from line to line.
fn shape(line: &Line, frags: &[Fragment], glyphs: &[Glyph], scratch: &mut [Vec<f32>; 3]) -> Shape {
    let [bottoms, heights, sizes] = scratch;
    bottoms.clear();
    heights.clear();
    sizes.clear();
    for f in line.frags.iter().filter_map(|&i| frags.get(i)) {
        for g in glyphs.get(f.glyphs.clone()).unwrap_or(&[]) {
            if !g.ch.is_whitespace() {
                bottoms.push(g.b.v1);
                heights.push(g.b.height());
                sizes.push(g.b.height().max(g.b.width()));
            }
        }
    }
    Shape {
        dir: line.dir,
        ink: line.ink,
        base: median(bottoms),
        height: median(heights),
        size: median(sizes),
    }
}

/// Hands over a line's characters and the whitespace between them.
fn emit_line(line: &Line, frags: &[Fragment], glyphs: &[Glyph], emit: &mut impl FnMut(LaidText)) {
    // The last non-blank character, and the whitespace read since.
    let mut prev: Option<&Glyph> = None;
    let mut blank: Vec<(char, CharPos)> = Vec::new();
    for f in line.frags.iter().filter_map(|&i| frags.get(i)) {
        let run = glyphs.get(f.glyphs.clone()).unwrap_or(&[]);
        for (k, g) in run.iter().enumerate() {
            let at = CharPos {
                span: f.at.span,
                ch: f.at.ch + k,
            };
            if g.ch.is_whitespace() {
                blank.push((g.ch, at));
                continue;
            }
            // Indentation the line starts with is kept.
            let sep = prev.map_or(Separator::Blank, |p| separator(p, g, &blank));
            match sep {
                Separator::None => {}
                Separator::Space => emit(LaidText::Space),
                Separator::Blank => {
                    for &(ch, at) in &blank {
                        emit(LaidText::Char { ch, at });
                    }
                }
            }
            blank.clear();
            emit(LaidText::Char { ch: g.ch, at });
            prev = Some(g);
        }
    }
    // Trailing whitespace (`blank`) is dropped.
}

/// What goes between neighbours on a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Separator {
    /// Nothing.
    None,
    /// A space the text does not have.
    Space,
    /// The whitespace the text has between them (maybe none).
    Blank,
}

/// What goes between neighbours `p` and `g` on a line, given the
/// whitespace characters `blank` that the text has between them.
fn separator(p: &Glyph, g: &Glyph, blank: &[(char, CharPos)]) -> Separator {
    let gap = g.b.u0 - p.b.u1;
    let height = p.b.height().min(g.b.height());
    if is_cjk(p.ch) && is_cjk(g.ch) {
        let apart = gap >= CJK_GAP * height;
        if blank.is_empty() {
            return if apart {
                Separator::Space
            } else {
                Separator::None
            };
        }
        if !apart && blank.iter().all(|&(c, _)| c == ' ') {
            return Separator::None;
        }
        return Separator::Blank;
    }
    if blank.is_empty() && gap > SPACE_GAP * height {
        return Separator::Space;
    }
    Separator::Blank
}

/// Characters of scripts written without spaces between words (Han, kana,
/// bopomofo) and the full-width punctuation and symbols set with them,
/// including the quotes, dashes and dots CJK text uses. Hangul is not here:
/// Korean puts spaces between words.
pub fn is_cjk(c: char) -> bool {
    // Fast path: nothing below the middle dot is CJK (ASCII, Latin-1).
    if (c as u32) < 0xB7 {
        return false;
    }
    matches!(
        c as u32,
        0x2E80..=0x2FDF // CJK and Kangxi radicals
            | 0x2FF0..=0x2FFF // ideographic description
            | 0x3000..=0x303F // CJK symbols and punctuation: 、。「」『』〈〉
            | 0x3040..=0x30FF // hiragana, katakana
            | 0x3100..=0x312F // bopomofo
            | 0x3190..=0x31FF // kanbun, bopomofo extended, strokes, katakana extensions
            | 0x3200..=0x33FF // enclosed CJK, CJK compatibility: ㈱ ㎡
            | 0x3400..=0x4DBF // extension A
            | 0x4E00..=0x9FFF // unified ideographs
            | 0xF900..=0xFAFF // compatibility ideographs
            | 0xFE10..=0xFE1F // vertical forms
            | 0xFE30..=0xFE4F // compatibility forms: ︵ ﹁
            | 0xFF00..=0xFFEF // full-width and half-width forms: ，：（）ＡＢＣ ｶﾀｶﾅ
            | 0x20000..=0x3FFFF // extensions B and later
    ) || matches!(
        c,
        '\u{00B7}' | '\u{2014}' | '\u{2015}' | '\u{2018}'..='\u{201F}' | '\u{2025}'..='\u{2027}'
    )
}

/// The value at index `len / 2` in sorted order (0 for none), found without
/// sorting. Most lines are one font on one baseline: all values alike.
fn median(values: &mut [f32]) -> f32 {
    let Some(&first) = values.first() else {
        return 0.0;
    };
    if values.iter().all(|v| v.to_bits() == first.to_bits()) {
        return first;
    }
    let mid = values.len() / 2;
    *values.select_nth_unstable_by(mid, f32::total_cmp).1
}

/// The break after each line but the last.
fn line_breaks(lines: &[Shape]) -> Vec<LineBreak> {
    let mut out = Vec::with_capacity(lines.len().saturating_sub(1));
    // The current block: the span of `u` it covers, and its line pairs as
    // (index into `out`, baseline spacing, similar heights).
    let mut extent = lines.first().map_or((0.0, 0.0), |l| (l.ink.u0, l.ink.u1));
    let mut pairs: Vec<(usize, f32, bool)> = Vec::new();
    for pair in lines.windows(2) {
        let [a, b] = pair else { continue };
        let spacing = b.base - a.base;
        let (lo, hi) = (a.height.min(b.height), a.height.max(b.height));
        let overlap = extent.1.min(b.ink.u1) - extent.0.max(b.ink.u0);
        // `b` continues down the same column as the block.
        let column = a.dir == b.dir
            && spacing > 0.5 * hi
            && overlap >= COLUMN_OVERLAP * (extent.1 - extent.0).min(b.ink.width());
        let size = a.size.max(b.size);
        if column && hi <= BLOCK_HEIGHT_RATIO * lo && spacing <= BLOCK_MAX_SPACING * size {
            pairs.push((out.len(), spacing, hi <= BODY_HEIGHT_RATIO * lo));
            out.push(LineBreak::Line);
            extent = (extent.0.min(b.ink.u0), extent.1.max(b.ink.u1));
        } else {
            mark_paragraphs(&pairs, &mut out);
            pairs.clear();
            // Further down the same column: a heading of another size, or a
            // wide gap. Anything else (the next column, other text) just
            // starts a new line.
            out.push(if column {
                LineBreak::Paragraph
            } else {
                LineBreak::Line
            });
            extent = (b.ink.u0, b.ink.u1);
        }
    }
    mark_paragraphs(&pairs, &mut out);
    out
}

/// Turns a block's line breaks into paragraph breaks where its lines are
/// clearly further apart than its usual spacing: the lower median spacing
/// of its lines of similar height, when there are enough of them.
fn mark_paragraphs(pairs: &[(usize, f32, bool)], out: &mut [LineBreak]) {
    let mut body: Vec<f32> = pairs
        .iter()
        .filter(|(_, _, similar)| *similar)
        .map(|(_, spacing, _)| *spacing)
        .collect();
    if body.len() < MIN_BODY_PAIRS {
        return;
    }
    body.sort_by(f32::total_cmp);
    let Some(&usual) = body.get((body.len() - 1) / 2) else {
        return;
    };
    for &(i, spacing, _) in pairs {
        if spacing > PARAGRAPH_SPACING * usual
            && let Some(b) = out.get_mut(i)
        {
            *b = LineBreak::Paragraph;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PageIndex;

    /// Horizontal text with its top-left corner at (x, y): CJK characters
    /// 10 pt wide, others 5 pt, all 10 pt high.
    fn span(text: &str, x: f32, y: f32) -> TextSpan {
        let mut cx = x;
        let char_bounds: Vec<PageRect> = text
            .chars()
            .map(|c| {
                let w = if is_cjk(c) { 10.0 } else { 5.0 };
                let r = PageRect::new(cx, y, cx + w, y + 10.0);
                cx = r.x1;
                r
            })
            .collect();
        let bounds = char_bounds
            .iter()
            .copied()
            .reduce(PageRect::union)
            .unwrap_or_default();
        TextSpan {
            text: text.into(),
            bounds,
            char_bounds,
        }
    }

    fn layer(spans: Vec<TextSpan>) -> TextLayer {
        TextLayer {
            page: PageIndex::FIRST,
            spans,
        }
    }

    fn pieces(l: &TextLayer, range: Range<CharPos>) -> Vec<LaidText> {
        let mut out = Vec::new();
        l.lay_out(range, |p| out.push(p));
        out
    }

    /// The laid-out text, with `|` for synthetic spaces and `¶` for line
    /// breaks (`¶¶` for paragraph breaks).
    fn written(l: &TextLayer, range: Range<CharPos>) -> String {
        pieces(l, range)
            .into_iter()
            .map(|p| match p {
                LaidText::Char { ch, .. } => ch.to_string(),
                LaidText::Space => "|".into(),
                LaidText::Break(LineBreak::Line) => "¶".into(),
                LaidText::Break(LineBreak::Paragraph) => "¶¶".into(),
            })
            .collect()
    }

    fn pos(span: usize, ch: usize) -> CharPos {
        CharPos { span, ch }
    }

    #[test]
    fn pieces_name_their_characters_in_reading_order() {
        // The second line is drawn first; the first line in two spans.
        let l = layer(vec![
            span("文件", 0.0, 14.0),
            span("公", 0.0, 0.0),
            span("文 a", 10.0, 0.0),
        ]);
        assert_eq!(
            pieces(&l, CharPos::START..CharPos::END),
            vec![
                LaidText::Char {
                    ch: '公',
                    at: pos(1, 0)
                },
                LaidText::Char {
                    ch: '文',
                    at: pos(2, 0)
                },
                LaidText::Char {
                    ch: ' ',
                    at: pos(2, 1)
                },
                LaidText::Char {
                    ch: 'a',
                    at: pos(2, 2)
                },
                LaidText::Break(LineBreak::Line),
                LaidText::Char {
                    ch: '文',
                    at: pos(0, 0)
                },
                LaidText::Char {
                    ch: '件',
                    at: pos(0, 1)
                },
            ]
        );
        // A range in the middle of spans keeps the positions of its ends.
        assert_eq!(written(&l, pos(0, 1)..pos(2, 1)), "公文¶件");
        let first = pieces(&l, pos(0, 1)..pos(2, 1))
            .into_iter()
            .find_map(|p| match p {
                LaidText::Char { ch: '件', at } => Some(at),
                _ => None,
            });
        assert_eq!(first, Some(pos(0, 1)));
    }

    #[test]
    fn spaces_and_breaks_are_marked() {
        // Words drawn apart get a synthetic space; a space the text has is
        // handed over as a character.
        let l = layer(vec![
            span("Hello", 0.0, 0.0),
            span("world", 28.0, 0.0),
            span("again here", 0.0, 12.0),
        ]);
        assert_eq!(
            written(&l, CharPos::START..CharPos::END),
            "Hello|world¶again here"
        );
        // Empty layers and empty ranges hand over nothing.
        assert!(
            pieces(
                &TextLayer::new(PageIndex::FIRST),
                CharPos::START..CharPos::END
            )
            .is_empty()
        );
        assert!(pieces(&l, pos(1, 2)..pos(1, 2)).is_empty());
        assert!(pieces(&l, pos(2, 0)..pos(0, 3)).is_empty(), "reversed");
    }

    #[test]
    fn lines_only_layout_differs_in_paragraph_breaks_alone() {
        // Three lines 12 pt apart, a wider gap, two more lines.
        let l = layer(
            [0.0, 12.0, 24.0, 48.0, 60.0]
                .into_iter()
                .enumerate()
                .map(|(i, y)| span(&format!("line {i}"), 0.0, y))
                .collect(),
        );
        let all = CharPos::START..CharPos::END;
        assert_eq!(
            written(&l, all.clone()),
            "line 0¶line 1¶line 2¶¶line 3¶line 4"
        );
        let mut lines_only = Vec::new();
        l.lay_out_lines(all.clone(), |p| lines_only.push(p));
        let as_lines: Vec<LaidText> = pieces(&l, all)
            .into_iter()
            .map(|p| match p {
                LaidText::Break(_) => LaidText::Break(LineBreak::Line),
                other => other,
            })
            .collect();
        assert_eq!(lines_only, as_lines);
    }

    #[test]
    fn whole_page_range_covers_every_span() {
        let l = layer(vec![span("ab", 0.0, 0.0), span("cd", 0.0, 12.0)]);
        let end = pos(1, 2);
        assert_eq!(
            pieces(&l, CharPos::START..CharPos::END),
            pieces(&l, CharPos::START..end)
        );
        assert_eq!(written(&l, CharPos::START..CharPos::END), "ab¶cd");
    }

    #[test]
    fn cjk_ranges() {
        for c in "中、。「』，：Ａｶかカㄅ㈱…‧\u{20000}".chars() {
            assert!(is_cjk(c), "{c}");
        }
        for c in "aZ1.,éЯ한 ".chars() {
            assert!(!is_cjk(c), "{c}");
        }
    }
}
