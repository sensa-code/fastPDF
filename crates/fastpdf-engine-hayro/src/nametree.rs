//! Name-tree lookups (ISO 32000-2 §7.9.6) for named destinations, bounded
//! per engine call.
//!
//! One call (the links of a page, the outline) may look up thousands of
//! names in the same tree, and a hostile tree can be cyclic, flat and huge,
//! or point every kid at one enormous object. So the tree is read lazily
//! and at most once per call. A node (the root, and every indirect node by
//! its object id) is resolved when a lookup first reaches it and keeps its
//! `/Limits`; its kids and its `/Names` pairs (as a hash index) are read
//! only when a lookup's key falls within those limits. Later lookups only
//! compare keys, so a flat array of thousands of names costs one pass in
//! total instead of one per lookup, and siblings outside a key's range are
//! never indexed. Direct (inline) kids are read again on every visit.
//! Values stay unresolved references (`DestResolver` resolves each once).
//!
//! Budgets per call: what is read (nodes, pairs and kids, counted once
//! each, and their bytes, which bounds the index's memory) and node
//! visits; per lookup: nodes visited. Once a budget is spent, lookups find
//! nothing and the destination stays unresolved.

use std::collections::{HashMap, HashSet};

use hayro::hayro_syntax::object::{
    Array, Dict, MaybeRef, Object, ObjectIdentifier, String as PdfString,
};
use hayro::hayro_syntax::xref::XRef;

use crate::items::items;

/// Nodes one lookup visits at most.
const MAX_NODES_PER_LOOKUP: usize = 10_000;
/// Node visits of all lookups of one call. Visiting a node already read is
/// a hash lookup and a key comparison.
const MAX_VISITS: usize = 1_000_000;
/// Nodes, `/Names` pairs and `/Kids` entries read in one call: far above
/// what the lookups of real trees read.
const MAX_ENTRIES: usize = 1 << 17;
/// Bytes of the nodes, keys and direct values read in one call.
const MAX_BYTES: usize = 32 << 20;

/// A value of the tree, kept unresolved: the caller resolves each
/// referenced object once (`DestResolver` remembers them by id).
#[derive(Clone)]
pub(crate) enum Value<'p> {
    Ref(ObjectIdentifier),
    Direct(Object<'p>),
}

/// A kid waiting on a lookup's stack.
#[derive(Clone)]
enum Kid<'p> {
    Indirect(ObjectIdentifier),
    Direct(Dict<'p>),
}

/// A node resolved during this call.
struct Node<'p> {
    dict: Dict<'p>,
    /// `/Limits`: the lowest and highest key below this node.
    limits: Option<(PdfString<'p>, PdfString<'p>)>,
    /// Read when a key first falls within the limits.
    contents: Option<Contents<'p>>,
}

#[derive(Default)]
struct Contents<'p> {
    /// `/Names`: the first value of each key.
    names: HashMap<PdfString<'p>, Value<'p>>,
    kids: Vec<Kid<'p>>,
}

/// What one call has read so far.
#[derive(Default)]
struct Budget {
    entries: usize,
    bytes: usize,
}

impl Budget {
    /// Counts one entry of `bytes`; false once a budget is spent.
    fn charge(&mut self, bytes: usize) -> bool {
        self.entries += 1;
        self.bytes = self.bytes.saturating_add(bytes);
        !self.spent()
    }

    fn spent(&self) -> bool {
        self.entries > MAX_ENTRIES || self.bytes > MAX_BYTES
    }
}

/// One name tree, read lazily during one engine call.
pub(crate) struct NameTree<'p> {
    xref: &'p XRef,
    root: Node<'p>,
    /// Indirect nodes resolved so far (`None`: not a dictionary).
    nodes: HashMap<ObjectIdentifier, Option<Node<'p>>>,
    budget: Budget,
    visits: usize,
}

impl<'p> NameTree<'p> {
    pub(crate) fn new(xref: &'p XRef, root: Dict<'p>) -> Self {
        let mut budget = Budget::default();
        let root = node(root, &mut budget);
        Self {
            xref,
            root,
            nodes: HashMap::new(),
            budget,
            visits: 0,
        }
    }

    /// The value stored under `key`, if the budgets allow finding it.
    pub(crate) fn lookup(&mut self, key: &[u8]) -> Option<Value<'p>> {
        let mut stack: Vec<Option<Kid<'p>>> = vec![None];
        let mut seen: HashSet<ObjectIdentifier> = HashSet::new();
        let mut visited = 0;
        while let Some(at) = stack.pop() {
            visited += 1;
            self.visits += 1;
            if visited > MAX_NODES_PER_LOOKUP || self.visits > MAX_VISITS {
                return None;
            }
            let mut direct;
            let node = match at {
                None => &mut self.root,
                Some(Kid::Indirect(id)) => {
                    if !seen.insert(id) {
                        continue;
                    }
                    if !self.nodes.contains_key(&id) {
                        // Once the budget is spent, nothing more is read.
                        let resolved = if self.budget.spent() {
                            None
                        } else {
                            self.xref
                                .get::<Dict<'p>>(id)
                                .map(|d| node(d, &mut self.budget))
                        };
                        self.nodes.insert(id, resolved);
                    }
                    match self.nodes.get_mut(&id).and_then(Option::as_mut) {
                        Some(node) => node,
                        None => continue,
                    }
                }
                Some(Kid::Direct(dict)) => {
                    direct = node(dict, &mut self.budget);
                    &mut direct
                }
            };
            if node
                .limits
                .as_ref()
                .is_some_and(|(lo, hi)| key < lo.as_bytes() || key > hi.as_bytes())
            {
                continue;
            }
            let contents = match node.contents.take() {
                Some(contents) => contents,
                None => read_contents(self.xref, &node.dict, &mut self.budget),
            };
            let found = contents.names.get(key).cloned();
            stack.extend(contents.kids.iter().cloned().map(Some));
            node.contents = Some(contents);
            if found.is_some() {
                return found;
            }
        }
        None
    }
}

/// A node with its limits, charged as one entry of its size (and of its
/// limits' strings, which may be references to strings elsewhere).
fn node<'p>(dict: Dict<'p>, budget: &mut Budget) -> Node<'p> {
    let mut limits = None;
    if budget.charge(dict.data().len()) {
        let read = dict.get::<Array<'p>>(b"Limits").and_then(|limits| {
            let mut it = limits.iter::<PdfString<'p>>();
            Some((it.next()?, it.next()?))
        });
        if let Some((lo, hi)) = read
            && budget.charge(lo.len() + hi.len())
        {
            limits = Some((lo, hi));
        }
    }
    Node {
        dict,
        limits,
        contents: None,
    }
}

/// Reads a node's names and kids; stops when the budget is spent (the node
/// then holds what was read). Every item read is charged, valid or not.
fn read_contents<'p>(xref: &'p XRef, dict: &Dict<'p>, budget: &mut Budget) -> Contents<'p> {
    let mut contents = Contents::default();
    if budget.spent() {
        return contents;
    }
    if let Some(names) = dict.get::<Array<'p>>(b"Names") {
        if !budget.charge(names.data().len()) {
            return contents;
        }
        let mut pairs = items(&names);
        while let Some(key) = pairs.next() {
            let value = pairs.next();
            let key = match key {
                MaybeRef::NotRef(Object::String(s)) => Some(s),
                MaybeRef::Ref(r) => xref.get::<PdfString<'p>>(r.into()),
                MaybeRef::NotRef(_) => None,
            };
            let value = value.map(|value| match value {
                MaybeRef::Ref(r) => (Value::Ref(r.into()), 0),
                MaybeRef::NotRef(object) => {
                    let size = object_size(&object);
                    (Value::Direct(object), size)
                }
            });
            let bytes = key.as_ref().map_or(0, |k| k.len()) + value.as_ref().map_or(0, |v| v.1);
            if !budget.charge(bytes) {
                return contents;
            }
            match (key, value) {
                (Some(key), Some((value, _))) => {
                    contents.names.entry(key).or_insert(value);
                }
                (_, None) => break,
                (None, Some(_)) => {}
            }
        }
    }
    if let Some(kids) = dict.get::<Array<'p>>(b"Kids") {
        if !budget.charge(kids.data().len()) {
            return contents;
        }
        for kid in items(&kids) {
            if !budget.charge(0) {
                return contents;
            }
            match kid {
                MaybeRef::Ref(r) => contents.kids.push(Kid::Indirect(r.into())),
                MaybeRef::NotRef(Object::Dict(d)) => contents.kids.push(Kid::Direct(d)),
                MaybeRef::NotRef(_) => {}
            }
        }
    }
    contents
}

/// Bytes a direct value spans in the file (what it may keep in memory).
fn object_size(object: &Object<'_>) -> usize {
    match object {
        Object::Array(a) => a.data().len(),
        Object::Dict(d) => d.data().len(),
        Object::String(s) => s.len(),
        _ => 16,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budgets_count_entries_and_bytes() {
        let mut budget = Budget::default();
        assert!(budget.charge(MAX_BYTES));
        assert!(!budget.charge(1));
        assert!(budget.spent());
        let mut budget = Budget::default();
        for _ in 0..MAX_ENTRIES {
            assert!(budget.charge(0));
        }
        assert!(!budget.charge(0));
    }
}
