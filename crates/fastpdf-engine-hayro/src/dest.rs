//! Targets of links and bookmarks: explicit destinations, named ones (the
//! catalog's `/Dests` dictionary and the `/Names /Dests` tree) and the
//! actions FastPDF understands (`GoTo`, `URI`). Every other action
//! (`Launch`, `GoToR`, `GoToE`, `JavaScript`, ...) is
//! [`LinkTarget::Unsupported`]: FastPDF never runs it.
//!
//! One [`DestResolver`] serves one engine call (a page's links, the
//! outline) and keeps that call bounded on hostile files: an object behind
//! a reference is read at most once per call and role (thousands of links
//! may point at one huge object), named destinations are remembered, the
//! name tree is read once (`crate::nametree`), and URIs have a size cap
//! per URI and per call.

use std::collections::HashMap;

use fastpdf_engine_api::{Destination, DestinationView, LinkTarget, PageIndex};
use hayro::hayro_syntax::object::{Array, Dict, MaybeRef, Name, Object, ObjectIdentifier};
use hayro::hayro_syntax::xref::XRef;
use hayro::kurbo::Rect;

use crate::document::{DocInner, Generation};
use crate::geometry::PageGeom;
use crate::items::items;
use crate::nametree::{NameTree, Value};

/// Longest URI passed on; longer ones are [`LinkTarget::Unsupported`]
/// (browsers stop near 2 KiB, the UI accepts at most 2048 bytes).
const MAX_URI_BYTES: usize = 8 * 1024;
/// URI bytes one call returns; later URIs are unsupported. Bounds the
/// answer (and its render-host reply) to about a megabyte.
const MAX_URI_BYTES_PER_CALL: usize = 1 << 20;
/// Objects behind references remembered per call.
const MAX_RESOLVED: usize = 32_768;
/// Named destinations remembered per call, and the bytes of their names.
const MAX_NAMED: usize = 10_000;
const MAX_NAMED_BYTES: usize = 1 << 20;

/// How an object behind a reference is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Role {
    /// A `/Dest` or a `GoTo` action's `/D`: an array, a name or a string.
    Dest,
    /// A value of the named destinations: an array or `<< /D [...] >>`.
    Named,
    /// `/D` of a `<< /D [...] >>` value: an array.
    Array,
    /// An action dictionary.
    Action,
    /// A `URI` action's `/URI` string.
    Uri,
}

/// Converts PDF destinations and actions to FastPDF targets.
pub(crate) struct DestResolver<'a, 'p> {
    doc: &'a DocInner,
    generation: &'p Generation,
    pub(crate) catalog: Option<Dict<'p>>,
    /// The catalog's `/Dests` dictionary (PDF 1.1), read on first use.
    dests: Option<Option<Dict<'p>>>,
    /// The `/Names /Dests` tree, created on first use.
    tree: Option<Option<NameTree<'p>>>,
    named: HashMap<Vec<u8>, Option<Destination>>,
    named_bytes: usize,
    resolved: HashMap<(ObjectIdentifier, Role), Option<LinkTarget>>,
    resolved_uri_bytes: usize,
    /// URI bytes returned by [`Self::target`] so far.
    uri_bytes: usize,
}

impl<'a, 'p> DestResolver<'a, 'p> {
    pub(crate) fn new(doc: &'a DocInner, generation: &'p Generation) -> Self {
        let xref = generation.pdf.xref();
        Self {
            doc,
            generation,
            catalog: xref.get::<Dict<'p>>(xref.root_id()),
            dests: None,
            tree: None,
            named: HashMap::new(),
            named_bytes: 0,
            resolved: HashMap::new(),
            resolved_uri_bytes: 0,
            uri_bytes: 0,
        }
    }

    fn xref(&self) -> &'p XRef {
        self.generation.pdf.xref()
    }

    /// The target of a link annotation or a bookmark: its `/Dest`, else
    /// its action `/A`. `None` when it has neither or they do not resolve.
    pub(crate) fn target(&mut self, dict: &Dict<'p>) -> Option<LinkTarget> {
        let target = if let Some(dest) = dict.get_raw::<Object<'p>>(b"Dest") {
            self.once(dest, Role::Dest)
        } else {
            self.once(dict.get_raw::<Object<'p>>(b"A")?, Role::Action)
        };
        if let Some(LinkTarget::Uri(uri)) = &target {
            self.uri_bytes = self.uri_bytes.saturating_add(uri.len());
            if self.uri_bytes > MAX_URI_BYTES_PER_CALL {
                return Some(LinkTarget::Unsupported);
            }
        }
        target
    }

    /// Reads `raw` in `role`; an object behind a reference once per call.
    fn once(&mut self, raw: MaybeRef<Object<'p>>, role: Role) -> Option<LinkTarget> {
        let id: ObjectIdentifier = match raw {
            MaybeRef::NotRef(object) => return self.read(object, role),
            MaybeRef::Ref(r) => r.into(),
        };
        if let Some(found) = self.resolved.get(&(id, role)) {
            return found.clone();
        }
        let found = self
            .xref()
            .get::<Object<'p>>(id)
            .and_then(|object| self.read(object, role));
        let uri = match &found {
            Some(LinkTarget::Uri(u)) => u.len(),
            _ => 0,
        };
        if self.resolved.len() < MAX_RESOLVED
            && self.resolved_uri_bytes + uri <= MAX_URI_BYTES_PER_CALL
        {
            self.resolved_uri_bytes += uri;
            self.resolved.insert((id, role), found.clone());
        }
        found
    }

    fn read(&mut self, object: Object<'p>, role: Role) -> Option<LinkTarget> {
        let internal = |d: Option<Destination>| d.map(LinkTarget::Internal);
        match (role, object) {
            (Role::Dest, object) => internal(self.resolve(&object)),
            (Role::Named, object) => internal(self.resolve_direct(&object)),
            (Role::Array, Object::Array(a)) => internal(self.explicit(&a)),
            (Role::Action, Object::Dict(action)) => self.action(&action),
            (Role::Uri, Object::String(s)) => Some(uri_target(s.as_bytes())),
            _ => None,
        }
    }

    fn destination(&mut self, raw: MaybeRef<Object<'p>>, role: Role) -> Option<Destination> {
        match self.once(raw, role)? {
            LinkTarget::Internal(d) => Some(d),
            _ => None,
        }
    }

    fn page_geom(&self, page: u32) -> Option<PageGeom> {
        let pages = self.generation.pdf.pages();
        pages.get(page as usize).map(PageGeom::from_page)
    }

    /// A destination value: an explicit array, or a name/string to look up.
    fn resolve(&mut self, value: &Object<'p>) -> Option<Destination> {
        match value {
            Object::Name(n) => self.named(n.as_ref()),
            Object::String(s) => self.named(s.as_bytes()),
            other => self.resolve_direct(other),
        }
    }

    /// Like `resolve` but never follows another name (no lookup loops):
    /// an array, or the `<< /D [...] >>` form found in `/Dests`.
    fn resolve_direct(&mut self, value: &Object<'p>) -> Option<Destination> {
        match value {
            Object::Array(a) => self.explicit(a),
            Object::Dict(d) => self.destination(d.get_raw::<Object<'p>>(b"D")?, Role::Array),
            _ => None,
        }
    }

    fn explicit(&self, array: &Array<'p>) -> Option<Destination> {
        let mut parts = items(array);
        let page = match parts.next()? {
            MaybeRef::Ref(r) => {
                let id: ObjectIdentifier = r.into();
                *self.doc.page_ids(self.generation).get(&id)?
            }
            // Remote-style destinations use a page number.
            MaybeRef::NotRef(Object::Number(n)) => {
                let i = n.as_f64();
                if !(0.0..f64::from(self.doc.page_count())).contains(&i) {
                    return None;
                }
                i as u32
            }
            MaybeRef::NotRef(_) => return None,
        };
        let geom = self.page_geom(page)?;
        let kind = match parts.next() {
            Some(MaybeRef::NotRef(Object::Name(n))) => n.as_str().to_owned(),
            _ => "Fit".to_owned(),
        };
        // At most four numbers follow (FitR); a huge array is not read.
        let nums: Vec<Option<f64>> = parts
            .take(4)
            .map(|item| match item {
                MaybeRef::NotRef(Object::Number(n)) => Some(n.as_f64()),
                _ => None,
            })
            .collect();
        let num = |i: usize| nums.get(i).copied().flatten().filter(|v| v.is_finite());
        let x = |v: f64| geom.page_point(v, 0.0).0;
        let y = |v: f64| geom.page_point(0.0, v).1;
        let view = match kind.as_str() {
            "XYZ" => DestinationView::Xyz {
                left: num(0).map(x),
                top: num(1).map(y),
                zoom: num(2).filter(|z| *z > 0.0).map(|z| z as f32),
            },
            "FitH" | "FitBH" => DestinationView::FitWidth { top: num(0).map(y) },
            "FitV" | "FitBV" => DestinationView::FitHeight {
                left: num(0).map(x),
            },
            "FitR" => match (num(0), num(1), num(2), num(3)) {
                (Some(l), Some(b), Some(r), Some(t)) => {
                    DestinationView::FitRect(geom.page_rect(Rect::new(l, b, r, t)))
                }
                _ => DestinationView::Fit,
            },
            _ => DestinationView::Fit,
        };
        Some(Destination {
            page: PageIndex::new(page),
            view,
        })
    }

    /// A named destination, looked up once per call.
    fn named(&mut self, name: &[u8]) -> Option<Destination> {
        if let Some(found) = self.named.get(name) {
            return found.clone();
        }
        let found = self.find_named(name);
        if self.named.len() < MAX_NAMED && self.named_bytes + name.len() <= MAX_NAMED_BYTES {
            self.named_bytes += name.len();
            self.named.insert(name.to_vec(), found.clone());
        }
        found
    }

    fn find_named(&mut self, name: &[u8]) -> Option<Destination> {
        // PDF 1.1 style: /Dests dictionary in the catalog.
        if self.dests.is_none() {
            let dests = self
                .catalog
                .as_ref()
                .and_then(|c| c.get::<Dict<'p>>(b"Dests"));
            self.dests = Some(dests);
        }
        let value = self
            .dests
            .as_ref()
            .and_then(Option::as_ref)
            .and_then(|dests| dests.get_raw::<Object<'p>>(name));
        if let Some(value) = value
            && let Some(d) = self.destination(value, Role::Named)
        {
            return Some(d);
        }
        // PDF 1.2+: /Names /Dests name tree.
        if self.tree.is_none() {
            let xref = self.xref();
            let tree = self
                .catalog
                .as_ref()
                .and_then(|c| c.get::<Dict<'p>>(b"Names"))
                .and_then(|n| n.get::<Dict<'p>>(b"Dests"))
                .map(|root| NameTree::new(xref, root));
            self.tree = Some(tree);
        }
        let value = self.tree.as_mut().and_then(Option::as_mut)?.lookup(name)?;
        let raw = match value {
            Value::Ref(id) => MaybeRef::Ref(id.into()),
            Value::Direct(object) => MaybeRef::NotRef(object),
        };
        self.destination(raw, Role::Named)
    }

    /// Target of an action dictionary.
    fn action(&mut self, action: &Dict<'p>) -> Option<LinkTarget> {
        let kind = action.get::<Name<'p>>(b"S")?;
        match kind.as_str() {
            "GoTo" => self.once(action.get_raw::<Object<'p>>(b"D")?, Role::Dest),
            "URI" => self.once(action.get_raw::<Object<'p>>(b"URI")?, Role::Uri),
            _ => Some(LinkTarget::Unsupported),
        }
    }
}

/// A `URI` action's string; too long ones are not passed on.
fn uri_target(bytes: &[u8]) -> LinkTarget {
    if bytes.len() > MAX_URI_BYTES {
        LinkTarget::Unsupported
    } else {
        LinkTarget::Uri(String::from_utf8_lossy(bytes).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_uris_are_not_passed_on() {
        assert_eq!(
            uri_target(b"https://example.invalid/"),
            LinkTarget::Uri("https://example.invalid/".into())
        );
        let long = vec![b'a'; MAX_URI_BYTES + 1];
        assert_eq!(uri_target(&long), LinkTarget::Unsupported);
        let exact = vec![b'a'; MAX_URI_BYTES];
        assert!(matches!(uri_target(&exact), LinkTarget::Uri(u) if u.len() == MAX_URI_BYTES));
    }
}
