//! The interactive form's field tree (`/AcroForm /Fields`, ISO 32000-2
//! §12.7.4): how many terminal fields it has, how many of them are
//! signature fields (`/FT /Sig`, inherited from a parent) and how many of
//! those hold a signature (`/V`, also inheritable: a dictionary, or a
//! reference, which is not resolved).
//!
//! `/Fields` is walked in its order, each entry down to its terminal
//! fields before the next. Some writers list kids in `/Fields` too: an
//! entry that names a `/Parent` waits until its parent reaches it (and is
//! walked on its own at the end if none does), and a field reached twice
//! is counted once; its parent is not a terminal field either way.
//!
//! The tree of a hostile file can be cyclic, deep or huge, and its
//! references can point at enormous objects, so the walk is iterative and
//! bounded like the page-label walk (`crate::labels`): every object is
//! resolved at most once (by identity), dictionaries (fields and widgets),
//! depth and the bytes of what is looked at have budgets, and a reference
//! that resolves to nothing of the needed type costs a fixed share of the
//! byte budget. Arrays are read up to their first unreadable item
//! (`crate::items`). Whenever something is cut, or a field is its own
//! ancestor, the counts are marked as lower bounds.

mod reads;

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use fastpdf_engine_api::FormSummary;
use hayro::hayro_syntax::object::{Array, Dict, MaybeRef, Object, ObjectIdentifier};
use hayro::hayro_syntax::xref::XRef;

use self::reads::{Kids, Path, has_value, signed};
use crate::items::items;

/// Field and widget dictionaries looked at, at most.
pub(super) const MAX_FORM_NODES: usize = 20_000;
/// Deepest field nesting followed (top-level fields are at depth 0).
const MAX_FORM_DEPTH: usize = 64;
/// Bytes of the form, field and widget dictionaries and kid arrays looked
/// at in total. Far above any real form.
const MAX_SCANNED_BYTES: usize = 32 << 20;
/// References that resolve to no object of the needed type, at most; each
/// costs `MAX_SCANNED_BYTES / MAX_REJECTED` of the budget.
const MAX_REJECTED: usize = 16;

/// The form of the document whose catalog is `catalog`.
pub(super) fn summary(xref: &XRef, catalog: &Dict<'_>) -> FormSummary {
    let Some(form) = catalog.get::<Dict<'_>>(b"AcroForm") else {
        return FormSummary::default();
    };
    let mut walk = Walk {
        xref,
        seen: HashSet::new(),
        types: HashMap::new(),
        field_ids: HashSet::new(),
        kid_arrays: HashMap::new(),
        waiting: HashMap::new(),
        nodes: 0,
        bytes: 0,
        summary: FormSummary {
            xfa: has_value(&form, b"XFA"),
            ..FormSummary::default()
        },
    };
    walk.run(&form);
    walk.summary
}

/// A field waiting on the walk's stack.
struct Node<'p> {
    dict: Dict<'p>,
    inherited: Inherited,
    /// The node and the fields above it.
    path: Rc<Path>,
}

/// What a parent passes on to its kids.
#[derive(Clone, Copy)]
struct Inherited {
    /// `/FT /Sig`, unless the field says otherwise.
    signature: bool,
    /// A signature as `/V`, likewise.
    signed: bool,
    depth: usize,
}

impl Inherited {
    const TOP: Self = Self {
        signature: false,
        signed: false,
        depth: 0,
    };
}

struct Walk<'p> {
    xref: &'p XRef,
    /// Indirect fields, widgets and arrays already resolved.
    seen: HashSet<ObjectIdentifier>,
    /// Indirect `/FT` names already read (whether they say `/Sig`).
    types: HashMap<ObjectIdentifier, Option<bool>>,
    /// Indirect fields queued.
    field_ids: HashSet<ObjectIdentifier>,
    /// Indirect `/Kids` arrays read: whether they held a field.
    kid_arrays: HashMap<ObjectIdentifier, bool>,
    /// Entries of `/Fields` that name a `/Parent`, until it reaches them.
    waiting: HashMap<ObjectIdentifier, Dict<'p>>,
    nodes: usize,
    bytes: usize,
    summary: FormSummary,
}

impl<'p> Walk<'p> {
    fn run(&mut self, form: &Dict<'p>) {
        if let Some(id) = form.obj_id() {
            self.seen.insert(id);
        }
        self.charge(form.data().len());
        let Kids::Array(fields, _) = self.kids(form, b"Fields") else {
            return;
        };
        // Every dictionary in /Fields is a field, with or without /T.
        let mut waiting = Vec::new();
        let mut entries = items(&fields);
        for item in entries.by_ref() {
            if self.cut() {
                return;
            }
            let (id, dict) = match item {
                MaybeRef::Ref(r) => {
                    let id: ObjectIdentifier = r.into();
                    // Reached from an earlier entry already.
                    if !self.seen.insert(id) {
                        continue;
                    }
                    let Some(dict) = self.resolve(id) else {
                        continue;
                    };
                    (Some(id), dict)
                }
                MaybeRef::NotRef(Object::Dict(dict)) => (None, dict),
                MaybeRef::NotRef(_) => continue,
            };
            self.nodes += 1;
            if let Some(id) = id
                && dict.contains_key(b"Parent")
            {
                self.waiting.insert(id, dict);
                waiting.push(id);
                continue;
            }
            self.descend(id, dict);
        }
        if entries.stopped() {
            self.summary.truncated = true;
        }
        // Entries whose parent never reached them stand on their own.
        for id in waiting {
            if self.spent() {
                return;
            }
            if let Some(dict) = self.waiting.remove(&id) {
                self.descend(Some(id), dict);
            }
        }
    }

    /// Walks the field `dict` (object `id`) down to its terminal fields.
    fn descend(&mut self, id: Option<ObjectIdentifier>, dict: Dict<'p>) {
        if let Some(id) = id {
            self.field_ids.insert(id);
        }
        let mut stack = vec![Node {
            dict,
            inherited: Inherited::TOP,
            path: Rc::new(Path { id, parent: None }),
        }];
        while let Some(node) = stack.pop() {
            if self.spent() {
                return;
            }
            let signature = self
                .field_type(&node.dict)
                .unwrap_or(node.inherited.signature);
            let signed = signed(&node.dict).unwrap_or(node.inherited.signed);
            let fields_below = match self.kids(&node.dict, b"Kids") {
                Kids::None => false,
                Kids::Read(held_fields) => held_fields,
                Kids::Array(..) if node.inherited.depth >= MAX_FORM_DEPTH => {
                    // Its kids are not looked at: it may or may not be a
                    // terminal field.
                    self.summary.truncated = true;
                    continue;
                }
                Kids::Array(kids, array_id) => {
                    let inherited = Inherited {
                        signature,
                        signed,
                        depth: node.inherited.depth + 1,
                    };
                    let held = self.queue(&kids, inherited, &node.path, &mut stack);
                    if let Some(array_id) = array_id {
                        self.kid_arrays.insert(array_id, held);
                    }
                    held
                }
            };
            if !fields_below {
                self.count(signature, signed);
            }
        }
    }

    /// A terminal field: counted, and as a signature field (signed or not)
    /// when it is one.
    fn count(&mut self, signature: bool, signed: bool) {
        let s = &mut self.summary;
        s.fields = s.fields.saturating_add(1);
        if signature {
            s.signature_fields = s.signature_fields.saturating_add(1);
            if signed {
                s.signatures = s.signatures.saturating_add(1);
            }
        }
    }

    /// Puts the fields among a field's kids (those with a name, `/T`; the
    /// others are its widgets) on the stack. True when the kids held a
    /// field (one reached before included), or when the budgets cut them
    /// short (the parent is then not counted).
    fn queue(
        &mut self,
        array: &Array<'p>,
        inherited: Inherited,
        path: &Rc<Path>,
        stack: &mut Vec<Node<'p>>,
    ) -> bool {
        let mut fields = false;
        let mut kids = items(array);
        for item in kids.by_ref() {
            if self.cut() {
                return true;
            }
            let (id, dict) = match item {
                MaybeRef::Ref(r) => {
                    let id: ObjectIdentifier = r.into();
                    if let Some(dict) = self.waiting.remove(&id) {
                        // A /Fields entry that waited for its parent.
                        (Some(id), dict)
                    } else if !self.seen.insert(id) {
                        // Read before: a field listed twice, a field that is
                        // its own ancestor (a cycle), or a widget.
                        if self.field_ids.contains(&id) {
                            fields = true;
                            if path.contains(id) {
                                self.summary.truncated = true;
                            }
                        }
                        continue;
                    } else {
                        let Some(dict) = self.resolve(id) else {
                            continue;
                        };
                        self.nodes += 1;
                        (Some(id), dict)
                    }
                }
                // Part of the array's bytes, already charged.
                MaybeRef::NotRef(Object::Dict(dict)) => {
                    self.nodes += 1;
                    (None, dict)
                }
                MaybeRef::NotRef(_) => continue,
            };
            if dict.contains_key(b"T") {
                fields = true;
                if let Some(id) = id {
                    self.field_ids.insert(id);
                }
                stack.push(Node {
                    dict,
                    inherited,
                    path: Rc::new(Path {
                        id,
                        parent: Some(Rc::clone(path)),
                    }),
                });
            }
        }
        if kids.stopped() {
            self.summary.truncated = true;
        }
        fields
    }
}
