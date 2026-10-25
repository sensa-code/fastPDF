//! What the field walk reads from one dictionary: its type, its value, its
//! kids, each at most once per object and charged to the budget.

use hayro::hayro_syntax::object::{Array, Dict, MaybeRef, Name, Null, ObjectIdentifier};

use super::{MAX_FORM_NODES, MAX_REJECTED, MAX_SCANNED_BYTES, Walk};

/// The fields from a node up to the top, by object id (a field written
/// directly into its parent has none).
pub(super) struct Path {
    pub(super) id: Option<ObjectIdentifier>,
    pub(super) parent: Option<std::rc::Rc<Path>>,
}

impl Path {
    /// Whether object `id` is this field or one above it (at most the
    /// walk's depth of links).
    pub(super) fn contains(&self, id: ObjectIdentifier) -> bool {
        let mut node = Some(self);
        while let Some(path) = node {
            if path.id == Some(id) {
                return true;
            }
            node = path.parent.as_deref();
        }
        false
    }
}

/// A dictionary's kids (or the form's fields).
pub(super) enum Kids<'p> {
    None,
    /// The array, and its object id when it is indirect.
    Array(Array<'p>, Option<ObjectIdentifier>),
    /// An indirect array read before for another field: whether it held a
    /// field.
    Read(bool),
}

/// Whether `key` has a value other than `null` (which counts as absent).
pub(super) fn has_value(dict: &Dict<'_>, key: &[u8]) -> bool {
    dict.contains_key(key) && !matches!(dict.get_raw::<Null>(key), Some(MaybeRef::NotRef(_)))
}

/// Whether a kid is a field rather than one of its parent's widgets: it
/// has a name (`/T`), or kids of its own (an unnamed field that groups
/// others; widgets have none).
pub(super) fn is_field(dict: &Dict<'_>) -> bool {
    dict.contains_key(b"T") || dict.contains_key(b"Kids")
}

/// Whether a field's own `/V` is a signature: a dictionary, or a reference
/// that is not resolved (the writer's rule; a signature can be large).
/// `None` when it has none (`null` counts as none): the parent's `/V`
/// decides.
pub(super) fn signed(field: &Dict<'_>) -> Option<bool> {
    has_value(field, b"V").then(|| field.get_raw::<Dict<'_>>(b"V").is_some())
}

impl<'p> Walk<'p> {
    /// Whether `/FT` names a signature field; `None` without one. An
    /// indirect name is resolved once per object and charged a fixed share
    /// of the budget whatever it turns out to be, like a rejected object:
    /// how much was read to find out cannot be told.
    pub(super) fn field_type(&mut self, field: &Dict<'p>) -> Option<bool> {
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
    /// time it is asked for. An indirect array read before (shared by two
    /// fields, or a cycle) answers whether it held a field; when that is
    /// not known yet, the counts become lower bounds.
    pub(super) fn kids(&mut self, dict: &Dict<'p>, key: &[u8]) -> Kids<'p> {
        match dict.get_raw::<Array<'p>>(key) {
            None => Kids::None,
            Some(MaybeRef::NotRef(array)) => Kids::Array(array, None),
            Some(MaybeRef::Ref(r)) => {
                let id: ObjectIdentifier = r.into();
                if !self.seen.insert(id) {
                    return match self.kid_arrays.get(&id) {
                        Some(held) => Kids::Read(*held),
                        None => {
                            self.summary.truncated = true;
                            Kids::Read(true)
                        }
                    };
                }
                match self.xref.get::<Array<'p>>(id) {
                    Some(array) => {
                        self.charge(array.data().len());
                        Kids::Array(array, Some(id))
                    }
                    None => {
                        // No kids, for every field that names this object.
                        self.reject();
                        self.kid_arrays.insert(id, false);
                        Kids::None
                    }
                }
            }
        }
    }

    /// The dictionary object `id` (already marked as seen); `None`, and
    /// the fixed charge, when it is no dictionary.
    pub(super) fn resolve(&mut self, id: ObjectIdentifier) -> Option<Dict<'p>> {
        match self.xref.get::<Dict<'p>>(id) {
            Some(dict) => {
                self.charge(dict.data().len());
                Some(dict)
            }
            None => {
                self.reject();
                None
            }
        }
    }

    /// True, with the counts marked as lower bounds, when no further
    /// dictionary may be looked at.
    pub(super) fn cut(&mut self) -> bool {
        if self.nodes >= MAX_FORM_NODES {
            self.summary.truncated = true;
            return true;
        }
        self.spent()
    }

    /// True, with the counts marked as lower bounds, once the byte budget
    /// is spent.
    pub(super) fn spent(&mut self) -> bool {
        let spent = self.bytes > MAX_SCANNED_BYTES;
        if spent {
            self.summary.truncated = true;
        }
        spent
    }

    pub(super) fn charge(&mut self, len: usize) {
        self.bytes = self.bytes.saturating_add(len);
    }

    /// A reference resolved to no object of the needed type: charges its
    /// fixed share of the budget.
    pub(super) fn reject(&mut self) {
        self.charge(MAX_SCANNED_BYTES / MAX_REJECTED);
    }
}
