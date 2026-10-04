//! Copy text: the selected characters, written the way the page lays them
//! out (spec §8: Copy).
//!
//! A selection is a range of characters in content order, and its
//! highlight shows exactly that range. Copy keeps those characters and only
//! decides their order and the whitespace between them:
//!
//! 1. **Direction.** Each span's glyphs advance right, down, left or up in
//!    page space. Its text is read in that direction's frame: `u` along the
//!    text, `v` toward the next line (down for horizontal text, leftward for
//!    vertical CJK). Rotated pages need no special case: a page with
//!    `/Rotate` whose text reads upright on screen draws that text rotated
//!    in page space, and that is the direction its glyphs advance in.
//! 2. **Lines.** The selected run of one span (a *fragment*) joins a line
//!    when it sits on the line's baseline and either directly follows one of
//!    the line's fragments in content order, at any distance (table cells,
//!    tab stops), or lies within a character height of one (a line drawn in
//!    pieces, out of order). Column gutters are wider, so columns stay apart.
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
//!    and plain spaces in them are dropped. `\n` between lines; an empty line
//!    where the lines of a column are clearly further apart than usual (a
//!    paragraph break). Hyphenated words are not joined: telling a line-end
//!    hyphen from a real one is too unreliable.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::ops::Range;

use fastpdf_engine_api::{PageRect, TextLayer, TextSpan};

use super::TextPos;

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

/// Text of the characters from `start` (inclusive) to `end` (exclusive).
pub(super) fn copy_text(layer: &TextLayer, start: TextPos, end: TextPos) -> String {
    let dirs = span_dirs(layer);
    let (glyphs, frags) = collect(layer, start, end, &dirs);
    let lines = build_lines(&frags);
    let laid: Vec<Laid> = order_lines(&lines, &furniture(layer, &dirs))
        .into_iter()
        .filter_map(|i| lines.get(i))
        .map(|line| lay(line, &frags, &glyphs))
        .collect();
    let breaks = line_breaks(&laid);
    let mut out = String::new();
    for (i, line) in laid.iter().enumerate() {
        if let Some(b) = i.checked_sub(1).and_then(|i| breaks.get(i)) {
            out.push_str(match b {
                Break::Line => "\n",
                Break::Paragraph => "\n\n",
            });
        }
        out.push_str(&line.text);
    }
    out
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
    let own: Vec<Option<Dir>> = layer.spans.iter().map(span_dir).collect();
    let mut weight = [0usize; 4];
    for (span, dir) in layer.spans.iter().zip(&own) {
        if let Some(dir) = dir {
            weight[*dir as usize] += span.char_count();
        }
    }
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

/// A span's direction: from the centre of its first non-blank character to
/// that of its last, when they are clearly apart.
fn span_dir(span: &TextSpan) -> Option<Dir> {
    let mut ends: Option<(PageRect, PageRect)> = None;
    for (c, r) in span.text.chars().zip(span.char_rects()) {
        if !c.is_whitespace() {
            let r = finite(r);
            ends = Some(ends.map_or((r, r), |(first, _)| (first, r)));
        }
    }
    let (a, b) = ends?;
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

/// A selected character and its box in its span's frame.
#[derive(Debug)]
struct Glyph {
    ch: char,
    b: Bx,
}

/// The selected characters of one span on one line.
#[derive(Debug)]
struct Fragment {
    dir: Dir,
    glyphs: Range<usize>,
    /// Union of the boxes of its non-blank characters.
    ink: Bx,
}

/// The selected characters in content order, cut into fragments. Runs of
/// whitespace alone make no fragment: gaps stand for them.
fn collect(
    layer: &TextLayer,
    start: TextPos,
    end: TextPos,
    dirs: &[Dir],
) -> (Vec<Glyph>, Vec<Fragment>) {
    let mut glyphs = Vec::new();
    let mut frags = Vec::new();
    let Some(last) = layer.spans.len().checked_sub(1) else {
        return (glyphs, frags);
    };
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
                    push_fragment(&mut frags, dir, begin..glyphs.len(), ink.take());
                    begin = glyphs.len();
                }
                ink = Some(ink.map_or(b, |i| i.union(b)));
                last_ink = Some(b);
            }
            glyphs.push(Glyph { ch, b });
        }
        push_fragment(&mut frags, dir, begin..glyphs.len(), ink);
    }
    (glyphs, frags)
}

fn push_fragment(frags: &mut Vec<Fragment>, dir: Dir, glyphs: Range<usize>, ink: Option<Bx>) {
    if let Some(ink) = ink {
        frags.push(Fragment { dir, glyphs, ink });
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
    let roles = roles(lines, furniture);
    let role = |i: usize| roles.get(i).copied().unwrap_or(Role::Body);
    let precedes = |y: usize, x: usize| match (lines.get(y), lines.get(x)) {
        (Some(a), Some(b)) => {
            above_in_column(a, b)
                || (a.dir == b.dir
                    && ((role(y) == Role::Header && role(x) != Role::Header)
                        || (role(x) == Role::Footer && role(y) != Role::Footer)))
        }
        _ => false,
    };
    let mut waiting: Vec<usize> = (0..n)
        .map(|x| (0..n).filter(|&y| precedes(y, x)).count())
        .collect();
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
        for x in 0..n {
            if !placed.get(x).copied().unwrap_or(true)
                && precedes(y, x)
                && let Some(w) = waiting.get_mut(x)
            {
                *w = w.saturating_sub(1);
                if *w == 0 {
                    ready.push(Reverse(x));
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

/// Whether `y` lies above `x` in the same column (same direction,
/// overlapping along the text), so that it must be read first.
fn above_in_column(y: &Line, x: &Line) -> bool {
    let (a, b) = (y.ink, x.ink);
    y.dir == x.dir
        && a.v_mid() < b.v_mid()
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
/// draw them first or last. Found on the whole page, so that a selection
/// that merely starts or ends with a heading is not affected.
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

/// A line's text and what placing breaks around it needs.
#[derive(Debug)]
struct Laid {
    text: String,
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

fn lay(line: &Line, frags: &[Fragment], glyphs: &[Glyph]) -> Laid {
    let mut text = String::new();
    let mut bottoms = Vec::new();
    let mut heights = Vec::new();
    let mut sizes = Vec::new();
    // The last non-blank character, and the whitespace read since.
    let mut prev: Option<&Glyph> = None;
    let mut blank = String::new();
    for f in line.frags.iter().filter_map(|&i| frags.get(i)) {
        for g in glyphs.get(f.glyphs.clone()).unwrap_or(&[]) {
            if g.ch.is_whitespace() {
                blank.push(g.ch);
                continue;
            }
            match prev {
                Some(p) => text.push_str(separator(p, g, &blank)),
                // Indentation the line starts with is kept.
                None => text.push_str(&blank),
            }
            blank.clear();
            text.push(g.ch);
            bottoms.push(g.b.v1);
            heights.push(g.b.height());
            sizes.push(g.b.height().max(g.b.width()));
            prev = Some(g);
        }
    }
    // Trailing whitespace (`blank`) is dropped.
    Laid {
        text,
        dir: line.dir,
        ink: line.ink,
        base: median(&mut bottoms),
        height: median(&mut heights),
        size: median(&mut sizes),
    }
}

/// What goes between neighbours `p` and `g` on a line, given the
/// whitespace characters `blank` that the text has between them.
fn separator<'a>(p: &Glyph, g: &Glyph, blank: &'a str) -> &'a str {
    let gap = g.b.u0 - p.b.u1;
    let height = p.b.height().min(g.b.height());
    if is_cjk(p.ch) && is_cjk(g.ch) {
        let apart = gap >= CJK_GAP * height;
        if blank.is_empty() {
            return if apart { " " } else { "" };
        }
        if !apart && blank.chars().all(|c| c == ' ') {
            return "";
        }
        return blank;
    }
    if blank.is_empty() && gap > SPACE_GAP * height {
        return " ";
    }
    blank
}

/// Characters of scripts written without spaces between words (Han, kana,
/// bopomofo) and the full-width punctuation and symbols set with them,
/// including the quotes, dashes and dots CJK text uses. Hangul is not here:
/// Korean puts spaces between words.
fn is_cjk(c: char) -> bool {
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

fn median(values: &mut [f32]) -> f32 {
    values.sort_by(f32::total_cmp);
    values.get(values.len() / 2).copied().unwrap_or(0.0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Break {
    Line,
    Paragraph,
}

/// The break after each laid line but the last.
fn line_breaks(laid: &[Laid]) -> Vec<Break> {
    let mut out = Vec::with_capacity(laid.len().saturating_sub(1));
    // The current block: the span of `u` it covers, and its line pairs as
    // (index into `out`, baseline spacing, similar heights).
    let mut extent = laid.first().map_or((0.0, 0.0), |l| (l.ink.u0, l.ink.u1));
    let mut pairs: Vec<(usize, f32, bool)> = Vec::new();
    for pair in laid.windows(2) {
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
            out.push(Break::Line);
            extent = (extent.0.min(b.ink.u0), extent.1.max(b.ink.u1));
        } else {
            mark_paragraphs(&pairs, &mut out);
            pairs.clear();
            // Further down the same column: a heading of another size, or a
            // wide gap. Anything else (the next column, other text) just
            // starts a new line.
            out.push(if column {
                Break::Paragraph
            } else {
                Break::Line
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
fn mark_paragraphs(pairs: &[(usize, f32, bool)], out: &mut [Break]) {
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
            *b = Break::Paragraph;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection::{PageSelection, select_all, select_range, selected_text};
    use fastpdf_engine_api::PageIndex;

    /// Font size of the test text: CJK characters are this wide, others
    /// half as wide.
    const EM: f32 = 10.0;

    fn advance(c: char) -> f32 {
        if is_cjk(c) { EM } else { EM / 2.0 }
    }

    fn span_of(text: &str, char_bounds: Vec<PageRect>) -> TextSpan {
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

    /// Horizontal text with its top-left corner at (x, y), characters
    /// `tracking` points apart (letter spacing).
    fn tracked(text: &str, x: f32, y: f32, tracking: f32) -> TextSpan {
        let mut cx = x;
        let boxes = text
            .chars()
            .map(|c| {
                let r = PageRect::new(cx, y, cx + advance(c), y + EM);
                cx = r.x1 + tracking;
                r
            })
            .collect();
        span_of(text, boxes)
    }

    fn span(text: &str, x: f32, y: f32) -> TextSpan {
        tracked(text, x, y, 0.0)
    }

    /// Text running down from (x, y): vertical writing, or text turned a
    /// quarter clockwise. The next line is to the left.
    fn down(text: &str, x: f32, y: f32) -> TextSpan {
        let mut cy = y;
        let boxes = text
            .chars()
            .map(|c| {
                let r = PageRect::new(x, cy, x + EM, cy + advance(c));
                cy = r.y1;
                r
            })
            .collect();
        span_of(text, boxes)
    }

    /// Text running up from (x, y), turned a quarter counter-clockwise as
    /// on a page shown with `/Rotate 90`. The next line is to the right.
    fn up(text: &str, x: f32, y: f32) -> TextSpan {
        let mut cy = y;
        let boxes = text
            .chars()
            .map(|c| {
                let r = PageRect::new(x, cy - advance(c), x + EM, cy);
                cy = r.y0;
                r
            })
            .collect();
        span_of(text, boxes)
    }

    /// Text running right to left from (x, y): upside down. The next line
    /// is above.
    fn upside_down(text: &str, x: f32, y: f32) -> TextSpan {
        let mut cx = x;
        let boxes = text
            .chars()
            .map(|c| {
                let r = PageRect::new(cx - advance(c), y, cx, y + EM);
                cx = r.x0;
                r
            })
            .collect();
        span_of(text, boxes)
    }

    fn layer(spans: Vec<TextSpan>) -> TextLayer {
        TextLayer {
            page: PageIndex::FIRST,
            spans,
        }
    }

    /// Select all, then copy.
    fn copy(spans: Vec<TextSpan>) -> String {
        let l = layer(spans);
        selected_text(&l, &select_all(&l))
    }

    fn selection(start: (usize, usize), end: (usize, usize)) -> PageSelection {
        PageSelection {
            page: PageIndex::FIRST,
            start: TextPos {
                span: start.0,
                ch: start.1,
            },
            end: TextPos {
                span: end.0,
                ch: end.1,
            },
            rects: Vec::new(),
        }
    }

    #[test]
    fn a_latin_line_split_into_spans_is_one_line() {
        // "The quick brown fox": the last part is drawn first, and the parts
        // are a word space (3 pt) apart.
        let spans = vec![
            span("brown fox", 46.0, 0.0),
            span("The", 0.0, 0.0),
            span("quick", 18.0, 0.0),
            span("jumps over", 0.0, 14.0),
        ];
        assert_eq!(copy(spans), "The quick brown fox\njumps over");

        // Superscripts and subscripts stay on their line, unspaced.
        let small =
            |text: &str, x: f32, y: f32| span_of(text, vec![PageRect::new(x, y, x + 3.5, y + 6.0)]);
        let formula = vec![
            span("E=mc", 0.0, 30.0),
            small("2", 20.0, 27.0),
            span(" and x", 24.0, 30.0),
            small("2", 54.0, 27.0),
            small("i", 57.5, 36.0),
        ];
        assert_eq!(copy(formula), "E=mc2 and x2i");
    }

    #[test]
    fn letter_spacing_and_word_gaps() {
        // Letter spacing (here 0.15 em) is not a word space.
        assert_eq!(copy(vec![tracked("Tracked", 0.0, 0.0, 1.5)]), "Tracked");
        // Words drawn apart without a space character get one.
        assert_eq!(
            copy(vec![span("Hello", 0.0, 0.0), span("world", 28.0, 0.0)]),
            "Hello world"
        );
        // A space already there is not doubled.
        assert_eq!(
            copy(vec![span("Hello ", 0.0, 0.0), span("world", 33.0, 0.0)]),
            "Hello world"
        );
        // Kerning pulls letters together.
        let kerned = span_of(
            "AV",
            vec![
                PageRect::new(0.0, 0.0, 6.0, 10.0),
                PageRect::new(5.0, 0.0, 11.0, 10.0),
            ],
        );
        assert_eq!(copy(vec![kerned]), "AV");
        // A gap inside one span counts too (engines that add no spaces).
        let apart = span_of(
            "ab",
            vec![
                PageRect::new(0.0, 0.0, 5.0, 10.0),
                PageRect::new(9.0, 0.0, 14.0, 10.0),
            ],
        );
        assert_eq!(copy(vec![apart]), "a b");
        // Trailing whitespace is dropped; leading (indentation) is kept.
        assert_eq!(copy(vec![span("  code();   ", 0.0, 0.0)]), "  code();");
    }

    #[test]
    fn mixed_chinese_and_latin() {
        let spans = vec![
            span("中文", 0.0, 0.0),
            span("PDF", 23.0, 0.0),
            span("閱讀器", 41.0, 0.0),
        ];
        assert_eq!(copy(spans), "中文 PDF 閱讀器");
        // Set solid: no space where there is no gap.
        assert_eq!(
            copy(vec![span("使用FastPDF閱讀，", 0.0, 0.0)]),
            "使用FastPDF閱讀，"
        );
    }

    #[test]
    fn chinese_has_no_spaces_but_table_cells_stay_apart() {
        // Justified: characters 0.2 em apart.
        assert_eq!(copy(vec![tracked("政府公文", 0.0, 0.0, 2.0)]), "政府公文");
        // Separate spans less than a character apart (a font switch).
        assert_eq!(
            copy(vec![span("中華", 0.0, 0.0), span("民國", 24.0, 0.0)]),
            "中華民國"
        );
        // A space an engine put into such a gap is dropped...
        let inserted = span_of(
            "龘、 倰",
            vec![
                PageRect::new(0.0, 0.0, 10.0, 10.0),
                PageRect::new(10.0, 0.0, 15.0, 10.0),
                PageRect::new(15.0, 2.0, 20.0, 8.0),
                PageRect::new(20.0, 0.0, 30.0, 10.0),
            ],
        );
        assert_eq!(copy(vec![inserted]), "龘、倰");
        // ...but ideographic spaces are the document's own.
        assert_eq!(copy(vec![span("檔　　號：", 0.0, 0.0)]), "檔　　號：");
        // Full-width punctuation counts as CJK.
        assert_eq!(
            copy(vec![
                span("（全形）", 0.0, 0.0),
                span("「引號」", 43.0, 0.0)
            ]),
            "（全形）「引號」"
        );
        // Table cells, a character width or more apart, keep a separator.
        let row = vec![
            span("項次", 0.0, 0.0),
            span("檢測項目", 40.0, 0.0),
            span("判定", 100.0, 0.0),
        ];
        assert_eq!(copy(row), "項次 檢測項目 判定");
        assert_eq!(
            copy(vec![span("第一行", 0.0, 0.0), span("第二行", 0.0, 14.0)]),
            "第一行\n第二行"
        );
    }

    /// A heading over two columns of three lines each (same baselines),
    /// drawn column by column.
    fn two_columns() -> Vec<TextSpan> {
        let mut spans = vec![span(
            "A heading that runs over both columns of the page",
            0.0,
            0.0,
        )];
        for (x, col) in [(0.0, 1), (150.0, 2)] {
            for line in 1..=3 {
                let y = 8.0 + 12.0 * line as f32;
                spans.push(span(&format!("column {col} line {line}"), x, y));
            }
        }
        spans
    }

    const TWO_COLUMNS: &str = "A heading that runs over both columns of the page\n\n\
        column 1 line 1\ncolumn 1 line 2\ncolumn 1 line 3\n\
        column 2 line 1\ncolumn 2 line 2\ncolumn 2 line 3";

    #[test]
    fn two_columns_read_column_by_column() {
        assert_eq!(copy(two_columns()), TWO_COLUMNS);

        // From the middle of the first column into the second.
        let l = layer(two_columns());
        let sel = select_range(&l, TextPos { span: 2, ch: 0 }, TextPos { span: 4, ch: 14 });
        assert_eq!(
            selected_text(&l, &sel),
            "column 1 line 2\ncolumn 1 line 3\ncolumn 2 line 1"
        );
        assert_eq!(sel.rects.len(), 3, "the highlight is unchanged");

        // A page number centred under both columns, drawn first, and the
        // heading drawn last.
        let mut spans = two_columns();
        let heading = spans.remove(0);
        spans.insert(0, span("1", 110.0, 70.0));
        spans.push(heading);
        assert_eq!(copy(spans), format!("{TWO_COLUMNS}\n1"));
    }

    #[test]
    fn lines_of_a_column_read_top_to_bottom() {
        let spans = vec![
            span("third", 0.0, 24.0),
            span("first", 0.0, 0.0),
            span("second", 0.0, 12.0),
        ];
        assert_eq!(copy(spans), "first\nsecond\nthird");
    }

    #[test]
    fn rotated_text_reads_in_its_own_direction() {
        // Up the page, as on pages shown with /Rotate 90; drawn in either
        // order.
        let first = up("First line", 100.0, 300.0);
        let second = up("Second line", 114.0, 300.0);
        assert_eq!(
            copy(vec![first.clone(), second.clone()]),
            "First line\nSecond line"
        );
        assert_eq!(copy(vec![second, first]), "First line\nSecond line");
        // Down the page (turned clockwise): the next line is to the left.
        assert_eq!(
            copy(vec![
                down("Second line", 186.0, 0.0),
                down("First line", 200.0, 0.0)
            ]),
            "First line\nSecond line"
        );
        // Vertical writing, columns right to left, under a horizontal title.
        let page = vec![
            span("Vertical", 0.0, 0.0),
            down("直排文字", 300.0, 20.0),
            down("第二行", 286.0, 20.0),
        ];
        assert_eq!(copy(page), "Vertical\n直排文字\n第二行");
        // Upside down: right to left, the next line above.
        assert_eq!(
            copy(vec![
                upside_down("Hello", 300.0, 500.0),
                upside_down("world", 300.0, 486.0)
            ]),
            "Hello\nworld"
        );
    }

    #[test]
    fn paragraphs_are_separated_by_an_empty_line() {
        let lines: Vec<TextSpan> = [0.0, 12.0, 24.0, 48.0, 60.0]
            .into_iter()
            .enumerate()
            .map(|(i, y)| span(&format!("line {}", i + 1), 0.0, y))
            .collect();
        assert_eq!(copy(lines), "line 1\nline 2\nline 3\n\nline 4\nline 5");
        // Two lines tell nothing about the usual spacing.
        assert_eq!(
            copy(vec![span("one", 0.0, 0.0), span("two", 0.0, 24.0)]),
            "one\ntwo"
        );
        // A larger heading over its text.
        let title = span_of(
            "Title",
            (0..5)
                .map(|i| {
                    let x = 10.0 * i as f32;
                    PageRect::new(x, 0.0, x + 10.0, 20.0)
                })
                .collect(),
        );
        let body = vec![title, span("body 1", 0.0, 30.0), span("body 2", 0.0, 42.0)];
        assert_eq!(copy(body), "Title\n\nbody 1\nbody 2");
    }

    #[test]
    fn empty_and_blank_selections_copy_nothing() {
        let empty = TextLayer::new(PageIndex::FIRST);
        assert_eq!(selected_text(&empty, &select_all(&empty)), "");
        let l = layer(vec![span("a   b", 0.0, 0.0)]);
        assert_eq!(selected_text(&l, &selection((0, 2), (0, 2))), "");
        assert_eq!(
            selected_text(&l, &selection((0, 1), (0, 4))),
            "",
            "only spaces"
        );
        assert_eq!(
            selected_text(&l, &selection((3, 0), (9, 2))),
            "",
            "past the end"
        );
        assert_eq!(
            selected_text(&l, &selection((0, 4), (0, 1))),
            "",
            "reversed"
        );
    }

    #[test]
    fn hostile_geometry_and_crowded_pages_finish() {
        let bad = PageRect {
            x0: f32::NAN,
            y0: f32::INFINITY,
            x1: f32::NEG_INFINITY,
            y1: f32::NAN,
        };
        let spans = vec![
            span_of("ab", vec![bad, bad]),
            span_of("c", vec![PageRect::default()]),
            span("ok", 0.0, 0.0),
            span_of("huge", vec![PageRect::new(-1e30, -1e30, 1e30, 1e30); 4]),
        ];
        let text = copy(spans);
        assert!(text.contains("ok"), "{text:?}");

        // Thousands of scattered characters (labels of a chart): every one
        // is kept, and ordering them stays fast enough.
        let mut seed = 7u32;
        let mut next = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32
        };
        let spans: Vec<TextSpan> = (0..6000)
            .map(|_| {
                let (x, y) = (next() * 600.0, next() * 800.0);
                span("x", x, y)
            })
            .collect();
        let text = copy(spans);
        assert_eq!(text.chars().filter(|&c| c == 'x').count(), 6000);
    }

    #[test]
    fn cjk_ranges() {
        for c in "中、。「』，：Ａｶかカㄅ㈱…‧\u{20000}".chars() {
            assert!(is_cjk(c), "{c}");
        }
        // Korean separates words with spaces.
        for c in "aZ1.,éЯ한 ".chars() {
            assert!(!is_cjk(c), "{c}");
        }
    }
}
