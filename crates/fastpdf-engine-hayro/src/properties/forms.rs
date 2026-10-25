//! The interactive form's field tree (`/AcroForm /Fields`, ISO 32000-2
//! §12.7.4): how many terminal fields it has, how many of them are
//! signature fields (`/FT /Sig`, inherited from a parent) and how many of
//! those hold a signature (`/V`, also inheritable: a dictionary, or a
//! reference, which is not resolved).
//!
//! The tree of a hostile file can be cyclic, deep or huge, and its
//! references can point at enormous objects, so the walk is iterative and
//! bounded like the page-label walk (`crate::labels`): every object is
//! resolved at most once (by identity), dictionaries (fields and widgets),
//! depth and the bytes of what is looked at have budgets, and a reference
//! that resolves to nothing of the needed type costs a fixed share of the
//! byte budget. Arrays are read up to their first unreadable item
//! (`crate::items`). Whenever something is cut, the counts are marked as
//! lower bounds.

use std::collections::{HashMap, HashSet};

use fastpdf_engine_api::FormSummary;
use hayro::hayro_syntax::object::{Array, Dict, MaybeRef, Name, Null, Object, ObjectIdentifier};
use hayro::hayro_syntax::xref::XRef;

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
        nodes: 0,
        bytes: 0,
        summary: FormSummary {
            xfa: form.contains_key(b"XFA"),
            ..FormSummary::default()
        },
    };
    walk.run(&form);
    walk.summary
}

/// A field waiting on the walk's stack.
struct Node<'p> {
    dict: Dict<'p>,
    /// `/FT /Sig`, inherited from the parent unless the field says.
    signature: bool,
    /// A signature as `/V`, inherited likewise.
    signed: bool,
    depth: usize,
}

/// What a parent passes on to its kids.
#[derive(Clone, Copy)]
struct Inherited {
    signature: bool,
    signed: bool,
    depth: usize,
}

/// Whether a field's own `/V` is a signature: a dictionary, or a reference
/// that is not resolved (the writer's rule; a signature can be large).
/// `None` when it has none (`null` counts as none): the parent's `/V`
/// decides.
fn signed(field: &Dict<'_>) -> Option<bool> {
    if !field.contains_key(b"V") || matches!(field.get_raw::<Null>(b"V"), Some(MaybeRef::NotRef(_)))
    {
        return None;
    }
    Some(field.get_raw::<Dict<'_>>(b"V").is_some())
}

struct Walk<'p> {
    xref: &'p XRef,
    /// Indirect fields, widgets and arrays already resolved.
    seen: HashSet<ObjectIdentifier>,
    /// Indirect `/FT` names already read (whether they say `/Sig`).
    types: HashMap<ObjectIdentifier, Option<bool>>,
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
        let Some(fields) = self.array(form, b"Fields") else {
            return;
        };
        let mut stack = Vec::new();
        // Every dictionary in /Fields is a field, with or without /T.
        let top = Inherited {
            signature: false,
            signed: false,
            depth: 0,
        };
        self.queue(&fields, top, true, &mut stack);
        while let Some(node) = stack.pop() {
            if self.spent() {
                self.summary.truncated = true;
                return;
            }
            let signature = self.field_type(&node.dict).unwrap_or(node.signature);
            let signed = signed(&node.dict).unwrap_or(node.signed);
            let has_fields_below = match self.array(&node.dict, b"Kids") {
                Some(_) if node.depth >= MAX_FORM_DEPTH => {
                    // Its kids are not looked at: it may or may not be a
                    // terminal field.
                    self.summary.truncated = true;
                    continue;
                }
                Some(kids) => {
                    let inherited = Inherited {
                        signature,
                        signed,
                        depth: node.depth + 1,
                    };
                    self.queue(&kids, inherited, false, &mut stack)
                }
                None => false,
            };
            if !has_fields_below {
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

    /// Puts the fields among `array`'s items on the stack: every dictionary
    /// at the top level, below it only those with a name (`/T`; the others
    /// are the parent's widgets). True when the array held a field, or when
    /// the budgets cut it short (the parent is then not counted).
    fn queue(
        &mut self,
        array: &Array<'p>,
        inherited: Inherited,
        top: bool,
        stack: &mut Vec<Node<'p>>,
    ) -> bool {
        let mut fields = false;
        let mut kids = items(array);
        for item in kids.by_ref() {
            if self.nodes >= MAX_FORM_NODES || self.spent() {
                self.summary.truncated = true;
                return true;
            }
            let dict = match item {
                MaybeRef::Ref(r) => {
                    let id: ObjectIdentifier = r.into();
                    if !self.seen.insert(id) {
                        continue;
                    }
                    match self.xref.get::<Dict<'p>>(id) {
                        Some(dict) => {
                            self.charge(dict.data().len());
                            dict
                        }
                        None => {
                            self.reject();
                            continue;
                        }
                    }
                }
                // Part of the array's bytes, already charged.
                MaybeRef::NotRef(Object::Dict(dict)) => dict,
                MaybeRef::NotRef(_) => continue,
            };
            self.nodes += 1;
            if top || dict.contains_key(b"T") {
                fields = true;
                stack.push(Node {
                    dict,
                    signature: inherited.signature,
                    signed: inherited.signed,
                    depth: inherited.depth,
                });
            }
        }
        if kids.stopped() {
            self.summary.truncated = true;
        }
        fields
    }

    /// Whether `/FT` names a signature field; `None` without one. An
    /// indirect name is resolved once per object and charged a fixed share
    /// of the budget whatever it turns out to be, like a rejected object:
    /// how much was read to find out cannot be told.
    fn field_type(&mut self, field: &Dict<'p>) -> Option<bool> {
        match field.get_raw::<Name<'p>>(b"FT")? {
            MaybeRef::NotRef(name) => Some(name.as_ref() == b"Sig"),
            MaybeRef::Ref(r) => {
                let id: ObjectIdentifier = r.into();
                if let Some(known) = self.types.get(&id) {
                    return *known;
                }
                let signature = self.xref.get::<Name<'p>>(id).map(|n| n.as_ref() == b"Sig");
                self.reject();
                self.types.insert(id, signature);
                signature
            }
        }
    }

    /// The array under `key`: a direct one, or an indirect one the first
    /// time it is asked for.
    fn array(&mut self, dict: &Dict<'p>, key: &[u8]) -> Option<Array<'p>> {
        match dict.get_raw::<Array<'p>>(key)? {
            MaybeRef::NotRef(array) => Some(array),
            MaybeRef::Ref(r) => {
                let id: ObjectIdentifier = r.into();
                if !self.seen.insert(id) {
                    return None;
                }
                let array = self.xref.get::<Array<'p>>(id);
                match &array {
                    Some(a) => self.charge(a.data().len()),
                    None => self.reject(),
                }
                array
            }
        }
    }

    fn charge(&mut self, len: usize) {
        self.bytes = self.bytes.saturating_add(len);
    }

    /// A reference resolved to no object of the needed type: charges its
    /// fixed share of the budget.
    fn reject(&mut self) {
        self.charge(MAX_SCANNED_BYTES / MAX_REJECTED);
    }

    fn spent(&self) -> bool {
        self.bytes > MAX_SCANNED_BYTES
    }
}
