//! Page labels: the catalog's `/PageLabels` number tree (ISO 32000-2
//! §7.9.7, §12.4.2), read with hayro-syntax's object API (hayro itself only
//! knows the key names).
//!
//! Number trees in hostile files can be cyclic, absurdly deep or huge, and
//! their entries can point at enormous objects. References are resolved as
//! the type the tree needs (a dictionary, an array, a string): hayro then
//! never reads a stream's data or its `/Length` (a stream object answers
//! with its dictionary), and what it returns is charged to a byte budget by
//! size. An object of another type is scanned to its end before hayro says
//! no, at a cost that cannot be measured from here, so every rejected
//! reference is charged a fixed share of the budget: at most
//! [`MAX_REJECTED`] per walk. The walk never resolves the same object
//! twice: kids, arrays and prefix strings are resolved once by identity,
//! label dictionaries are remembered, and `/S` and `/St` are only read when
//! they are direct. Kids are resolved one at a time when the walk reaches
//! them, after the budgets are checked. Nodes, depth, key/value pairs and
//! the bytes of everything looked at have budgets. Whatever is malformed is
//! skipped; the answer is always a normalized [`PageLabels`].

use std::collections::{BTreeMap, HashMap, HashSet};

use fastpdf_engine_api::display::clean_display;
use fastpdf_engine_api::{PageIndex, PageLabelRange, PageLabelStyle, PageLabels};
use hayro::hayro_syntax::Pdf;
use hayro::hayro_syntax::object::{
    Array, Dict, MaybeRef, Name, Object, ObjectIdentifier, String as PdfString,
};
use hayro::hayro_syntax::xref::XRef;

use crate::nav::decode_text_string;

/// Tree nodes visited at most (the same budget as name trees, nav.rs).
const MAX_NODES: usize = 10_000;
/// Deepest node visited; real trees are a few levels deep.
const MAX_DEPTH: usize = 32;
/// Key/value pairs looked at in total, valid or not.
const MAX_PAIRS: usize = 131_072;
/// Bytes of nodes, arrays, value dictionaries and prefix strings looked at
/// in total. Far above any real number tree.
const MAX_SCANNED_BYTES: usize = 32 << 20;
/// References that resolve to no object of the needed type (or to nothing),
/// at most; each costs `MAX_SCANNED_BYTES / MAX_REJECTED` of the budget.
const MAX_REJECTED: usize = 16;
/// Distinct indirect label dictionaries (and indirect prefixes) read at
/// most; real documents have a handful. References to further objects are
/// skipped.
const MAX_INDIRECT_VALUES: usize = 2 * PageLabels::MAX_RANGES;
/// Bytes of one `/P` string decoded at most: enough for
/// [`PageLabels::MAX_PREFIX_BYTES`] of text in any text-string encoding.
const MAX_PREFIX_SOURCE_BYTES: usize = 4 * PageLabels::MAX_PREFIX_BYTES;

/// The page labels of `pdf`, normalized for `page_count` pages. Empty when
/// the document has none.
pub(crate) fn page_labels(pdf: &Pdf, page_count: u32) -> PageLabels {
    let xref = pdf.xref();
    let root = xref
        .get::<Dict<'_>>(xref.root_id())
        .and_then(|catalog| catalog.get::<Dict<'_>>(b"PageLabels"));
    let mut walk = Walk {
        pdf,
        page_count,
        ranges: BTreeMap::new(),
        values: HashMap::new(),
        prefixes: HashMap::new(),
        resolved: HashSet::new(),
        nodes: 0,
        pairs: 0,
        bytes: 0,
    };
    if let Some(root) = root {
        walk.run(root);
    }
    let ranges = walk.ranges.into_iter().map(|(page, entry)| PageLabelRange {
        first_page: PageIndex::new(page),
        style: entry.style,
        prefix: entry.prefix,
        start: entry.start,
    });
    PageLabels::new(ranges.collect(), page_count)
}

/// Style, prefix and start of one label dictionary.
#[derive(Clone)]
struct Entry {
    style: PageLabelStyle,
    prefix: String,
    start: u32,
}

/// A kid waiting on the walk's stack; references are resolved when popped.
enum Pending<'p> {
    Direct(Dict<'p>),
    Indirect(ObjectIdentifier),
}

struct Walk<'p> {
    pdf: &'p Pdf,
    page_count: u32,
    /// By first page; at most `PageLabels::MAX_RANGES`, the lowest pages.
    ranges: BTreeMap<u32, Entry>,
    /// Indirect label dictionaries already read (`None`: not a label).
    values: HashMap<ObjectIdentifier, Option<Entry>>,
    /// Indirect `/P` strings already read, cleaned.
    prefixes: HashMap<ObjectIdentifier, String>,
    /// Indirect nodes and arrays already resolved (or queued).
    resolved: HashSet<ObjectIdentifier>,
    nodes: usize,
    pairs: usize,
    bytes: usize,
}

impl<'p> Walk<'p> {
    /// Adds `len` looked-at bytes; false once the budget is spent.
    fn charge(&mut self, len: usize) -> bool {
        self.bytes = self.bytes.saturating_add(len);
        self.bytes <= MAX_SCANNED_BYTES
    }

    fn run(&mut self, root: Dict<'p>) {
        if let Some(id) = root.obj_id() {
            self.resolved.insert(id);
        }
        let mut stack = vec![(Pending::Direct(root), 0usize)];
        while let Some((pending, depth)) = stack.pop() {
            if self.nodes >= MAX_NODES || self.bytes > MAX_SCANNED_BYTES {
                return;
            }
            self.nodes += 1;
            let node = match pending {
                Pending::Direct(dict) => {
                    if !self.charge(dict.data().len()) {
                        return;
                    }
                    dict
                }
                Pending::Indirect(id) => match self.xref().get::<Dict<'p>>(id) {
                    Some(dict) => {
                        if !self.charge(dict.data().len()) {
                            return;
                        }
                        dict
                    }
                    None => {
                        self.reject();
                        continue;
                    }
                },
            };
            if let Some(nums) = self.array(&node, b"Nums") {
                if !self.charge(nums.data().len()) {
                    return;
                }
                self.read_nums(&nums);
            }
            if depth >= MAX_DEPTH {
                continue;
            }
            let Some(kids) = self.array(&node, b"Kids") else {
                continue;
            };
            if !self.charge(kids.data().len()) {
                return;
            }
            let room = MAX_NODES.saturating_sub(self.nodes + stack.len());
            let queued = stack.len();
            for kid in kids.raw_iter().take(room) {
                let pending = match kid {
                    MaybeRef::Ref(r) => {
                        let id: ObjectIdentifier = r.into();
                        if !self.resolved.insert(id) {
                            continue;
                        }
                        Pending::Indirect(id)
                    }
                    MaybeRef::NotRef(Object::Dict(dict)) => Pending::Direct(dict),
                    MaybeRef::NotRef(_) => continue,
                };
                stack.push((pending, depth + 1));
            }
            // Leftmost kid on top: pages in document order.
            stack[queued..].reverse();
        }
    }

    /// The array under `key`: a direct one, or an indirect one the first
    /// time it is asked for.
    fn array(&mut self, dict: &Dict<'p>, key: &[u8]) -> Option<Array<'p>> {
        match dict.get_raw::<Array<'p>>(key)? {
            MaybeRef::NotRef(array) => Some(array),
            MaybeRef::Ref(r) => {
                let id: ObjectIdentifier = r.into();
                if !self.resolved.insert(id) {
                    return None;
                }
                let array = self.xref().get::<Array<'p>>(id);
                if array.is_none() {
                    self.reject();
                }
                array
            }
        }
    }

    fn xref(&self) -> &'p XRef {
        self.pdf.xref()
    }

    /// A reference resolved to no object of the needed type: charges its
    /// fixed share of the budget.
    fn reject(&mut self) {
        self.charge(MAX_SCANNED_BYTES / MAX_REJECTED);
    }

    fn read_nums(&mut self, nums: &Array<'p>) {
        let mut items = nums.raw_iter();
        while self.pairs < MAX_PAIRS && self.bytes <= MAX_SCANNED_BYTES {
            let (Some(key), Some(value)) = (items.next(), items.next()) else {
                return;
            };
            self.pairs += 1;
            let Some(page) = page_key(&key, self.page_count) else {
                continue;
            };
            // The first range given for a page wins; ranges past the kept
            // ones would be dropped by `PageLabels::new` anyway.
            let full = self.ranges.len() >= PageLabels::MAX_RANGES;
            if self.ranges.contains_key(&page)
                || (full && self.ranges.last_key_value().is_some_and(|(k, _)| *k < page))
            {
                continue;
            }
            if let Some(entry) = self.value(value) {
                self.ranges.insert(page, entry);
                if self.ranges.len() > PageLabels::MAX_RANGES {
                    self.ranges.pop_last();
                }
            }
        }
    }

    /// The label dictionary a value stands for, if it is one.
    fn value(&mut self, value: MaybeRef<Object<'p>>) -> Option<Entry> {
        match value {
            // Part of the `/Nums` array, whose bytes are already charged.
            MaybeRef::NotRef(Object::Dict(dict)) => self.entry(&dict),
            MaybeRef::NotRef(_) => None,
            MaybeRef::Ref(r) => {
                let id: ObjectIdentifier = r.into();
                if let Some(known) = self.values.get(&id) {
                    return known.clone();
                }
                if self.values.len() >= MAX_INDIRECT_VALUES {
                    return None;
                }
                let entry = match self.xref().get::<Dict<'p>>(id) {
                    Some(dict) if self.charge(dict.data().len()) => self.entry(&dict),
                    Some(_) => None,
                    None => {
                        self.reject();
                        None
                    }
                };
                self.values.insert(id, entry.clone());
                entry
            }
        }
    }

    fn entry(&mut self, dict: &Dict<'p>) -> Option<Entry> {
        // A style or start behind a reference is not followed: real files
        // write them directly, and following one could scan a huge object.
        let style = match dict.get_raw::<Name<'p>>(b"S") {
            Some(MaybeRef::NotRef(name)) => match name.as_str() {
                "D" => PageLabelStyle::Decimal,
                "R" => PageLabelStyle::UpperRoman,
                "r" => PageLabelStyle::LowerRoman,
                "A" => PageLabelStyle::UpperLetters,
                "a" => PageLabelStyle::LowerLetters,
                // Unknown styles keep the prefix and drop the number.
                _ => PageLabelStyle::None,
            },
            _ => PageLabelStyle::None,
        };
        let start = match dict.get_raw::<i64>(b"St") {
            Some(MaybeRef::NotRef(st)) => st.clamp(1, i64::from(u32::MAX)) as u32,
            _ => 1,
        };
        let prefix = match dict.get_raw::<PdfString<'p>>(b"P") {
            // Part of the dictionary's bytes, already charged.
            Some(MaybeRef::NotRef(text)) => prefix_text(&text),
            Some(MaybeRef::Ref(r)) => self.indirect_prefix(r.into())?,
            None => String::new(),
        };
        Some(Entry {
            style,
            prefix,
            start,
        })
    }

    /// An indirect `/P`, read once per object; `None` once the byte budget
    /// is spent.
    fn indirect_prefix(&mut self, id: ObjectIdentifier) -> Option<String> {
        if let Some(known) = self.prefixes.get(&id) {
            return Some(known.clone());
        }
        if self.prefixes.len() >= MAX_INDIRECT_VALUES {
            return Some(String::new());
        }
        let text = match self.xref().get::<PdfString<'p>>(id) {
            Some(text) if self.charge(text.as_bytes().len()) => prefix_text(&text),
            Some(_) => return None,
            None => {
                self.reject();
                String::new()
            }
        };
        self.prefixes.insert(id, text.clone());
        Some(text)
    }
}

/// A `/P` string decoded (at most its first bytes) and made safe to show.
fn prefix_text(text: &PdfString<'_>) -> String {
    let raw = text.as_bytes();
    let raw = raw.get(..MAX_PREFIX_SOURCE_BYTES).unwrap_or(raw);
    clean_display(&decode_text_string(raw), PageLabels::MAX_PREFIX_BYTES)
}

/// A number-tree key as a page index: a direct, finite, integral number
/// within the document.
fn page_key(key: &MaybeRef<Object<'_>>, page_count: u32) -> Option<u32> {
    let MaybeRef::NotRef(Object::Number(n)) = key else {
        return None;
    };
    let k = n.as_f64();
    (k.is_finite() && k.fract() == 0.0 && (0.0..f64::from(page_count)).contains(&k))
        .then_some(k as u32)
}
