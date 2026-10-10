//! Rewriting documents (ADR 0013 §3).
//!
//! An output is written from scratch: a catalog, a flat page tree, the
//! picked pages and every object they reach, copied from the sources and
//! renumbered. What nothing reaches is not written: deleted pages, the
//! content of earlier revisions, orphans. Dictionaries and arrays are copied
//! as the bytes they are stored as, with only their references renumbered,
//! so numbers, strings and names stay exactly as they were (sources are
//! unencrypted, see `super::execute`); stream data is copied undecoded.
//!
//! References to pages that the output does not have become `null`
//! (outline entries, links and structure elements of deleted pages), and so
//! do references to annotations found only on such pages: a deleted page's
//! form fields, comments and their values must not come back through the
//! structure tree or anything else that points at them. Form fields without
//! a widget on a kept page leave the form, and structure elements whose
//! content is only on deleted pages leave the structure tree (their
//! alternate and actual texts are the deleted pages' text). References to
//! the source's page tree nodes become `null` too and, when the pages come
//! from several documents, references to a source's catalog: a
//! single-source output keeps its catalog (outline, forms, names,
//! metadata, structure tree); a merged one gets the sources' form fields
//! and layers.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use fastpdf_engine_api::Rotation;
use fastpdf_engine_api::write::{PagePick, Stamp, WriteError};
use hayro::hayro_syntax::object::{Dict, Number, Object, ObjectIdentifier, Rect, Stream};
use hayro::hayro_syntax::page::{Page, Rotation as HayroRotation};
use hayro::hayro_syntax::{Pdf, PdfVersion};

use super::image::{self, ImageObject};
use super::syntax::{self, RawRef};
use crate::geometry::PageGeom;

/// Most objects an output may have: a runaway object graph ends the job.
const MAX_OBJECTS: usize = 10_000_000;
const CATALOG: u32 = 1;
const PAGES_ROOT: u32 = 2;

/// A source document, parsed.
pub(crate) struct Source {
    pub(crate) bytes: Arc<Vec<u8>>,
    pdf: Pdf,
    /// Object number of every page, in page order.
    page_numbers: Vec<u32>,
    page_of: HashMap<u32, usize>,
    /// Page tree nodes above the pages, the root included.
    tree: HashSet<u32>,
    /// The pages each annotation (object number) is listed on.
    annotation_pages: HashMap<u32, Vec<usize>>,
    catalog: ObjectIdentifier,
}

impl Source {
    pub(crate) fn open(bytes: Vec<u8>) -> Result<Self, String> {
        let bytes = Arc::new(bytes);
        let pdf = Pdf::new(Arc::clone(&bytes)).map_err(|e| format!("cannot parse: {e:?}"))?;
        let mut page_numbers = Vec::new();
        let mut page_of = HashMap::new();
        let mut tree = HashSet::new();
        let mut annotation_pages: HashMap<u32, Vec<usize>> = HashMap::new();
        for (index, page) in pdf.pages().iter().enumerate() {
            for number in annotations(&pdf, page.raw()) {
                annotation_pages.entry(number).or_default().push(index);
            }
            let number = page
                .raw()
                .obj_id()
                .and_then(|id| u32::try_from(id.obj_number).ok())
                .ok_or("a page is not an indirect object")?;
            page_numbers.push(number);
            page_of.insert(number, index);
            let mut node = page.raw().get::<Dict<'_>>(b"Parent");
            for _ in 0..64 {
                let Some(dict) = node else { break };
                let Some(n) = dict
                    .obj_id()
                    .and_then(|id| u32::try_from(id.obj_number).ok())
                else {
                    break;
                };
                if !tree.insert(n) {
                    break;
                }
                node = dict.get::<Dict<'_>>(b"Parent");
            }
        }
        if page_numbers.is_empty() {
            return Err("no pages".into());
        }
        let catalog = pdf.xref().root_id();
        Ok(Self {
            bytes,
            pdf,
            page_numbers,
            page_of,
            tree,
            annotation_pages,
            catalog,
        })
    }

    pub(crate) fn page_count(&self) -> usize {
        self.page_numbers.len()
    }

    fn version(&self) -> &'static str {
        match self.pdf.version() {
            PdfVersion::Pdf20 => "2.0",
            _ => "1.7",
        }
    }
}

/// Whether a structure element's kid is a marked-content or object
/// reference rather than another element.
fn is_content_reference(dict: &[u8]) -> bool {
    matches!(
        syntax::dict_value(dict, b"Type").map(<[u8]>::trim_ascii),
        Some(b"/MCR" | b"/OBJR")
    )
}

/// A field name with a number appended (`name-2`), in the name's own
/// encoding (UTF-16 when it starts with a byte order mark).
fn suffixed(name: &[u8], n: u32) -> Vec<u8> {
    let suffix = format!("-{n}");
    let mut out = name.to_vec();
    if name.starts_with(&[0xFE, 0xFF]) {
        for unit in suffix.encode_utf16() {
            out.extend_from_slice(&unit.to_be_bytes());
        }
    } else {
        out.extend_from_slice(suffix.as_bytes());
    }
    out
}

/// The references among the elements of `array`.
fn references_in(array: &[u8]) -> Vec<RawRef> {
    syntax::array_elements(array)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| syntax::single_reference(&array[e]))
        .collect()
}

/// Object numbers of the annotations a page lists in `/Annots` (an array
/// or a reference to one).
fn annotations(pdf: &Pdf, page: &Dict<'_>) -> Vec<u32> {
    let Some(raw) = syntax::dict_value(page.data(), b"Annots") else {
        return Vec::new();
    };
    let array: Vec<u8> = match syntax::single_reference(raw) {
        Some(r) => match pdf.xref().get::<Object<'_>>(object_id(r)) {
            Some(Object::Array(a)) => a.data().to_vec(),
            _ => return Vec::new(),
        },
        None => raw.to_vec(),
    };
    syntax::array_elements(&array)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| syntax::single_reference(&array[e]).map(|r| r.0))
        .collect()
}

fn object_id((number, generation): RawRef) -> ObjectIdentifier {
    ObjectIdentifier::new(
        i32::try_from(number).unwrap_or(i32::MAX),
        i32::from(generation),
    )
}

/// A new document made of `picks`.
pub(crate) fn pages(sources: &[Source], picks: &[PagePick]) -> Result<Vec<u8>, WriteError> {
    for pick in picks {
        let source = sources
            .get(pick.source)
            .ok_or_else(|| WriteError::Job("page of a missing source".into()))?;
        if pick.page.as_usize() >= source.page_count() {
            return Err(WriteError::NoSuchPage {
                source: pick.source,
                page: pick.page,
            });
        }
    }
    let first = picks
        .first()
        .map(|p| p.source)
        .ok_or_else(|| WriteError::Job("no pages".into()))?;
    let single = picks.iter().all(|p| p.source == first).then_some(first);
    // Page labels count pages: they only stay right for the same pages in
    // the same order.
    let keep_labels = single.is_some()
        && picks.len() == sources[first].page_count()
        && picks
            .iter()
            .enumerate()
            .all(|(i, p)| p.page.as_usize() == i);
    let version = if picks.iter().any(|p| sources[p.source].version() == "2.0") {
        "2.0"
    } else {
        "1.7"
    };

    let mut w = Writer::new(sources, single, version);
    let catalog = w.alloc()?;
    let root = w.alloc()?;
    debug_assert_eq!((catalog, root), (CATALOG, PAGES_ROOT));
    let mut numbers = Vec::with_capacity(picks.len());
    for pick in picks {
        let n = w.alloc()?;
        w.pages
            .entry((pick.source, pick.page.as_usize()))
            .or_insert(n);
        numbers.push(n);
    }
    w.write_catalog(keep_labels)?;
    let kids: Vec<String> = numbers.iter().map(|n| format!("{n} 0 R")).collect();
    w.object(
        PAGES_ROOT,
        format!(
            "<</Type /Pages/Kids [{}]/Count {}>>",
            kids.join(" "),
            picks.len()
        )
        .as_bytes(),
    );
    for (pick, &n) in picks.iter().zip(&numbers) {
        w.write_page(pick, n)?;
    }
    w.drain()?;
    let info = w.write_info(single.unwrap_or(first))?;
    w.finish(info)
}

struct Writer<'s> {
    sources: &'s [Source],
    out: Vec<u8>,
    /// Where each object starts (index: number - 1); `None` until written.
    offsets: Vec<Option<usize>>,
    copied: HashMap<(usize, u32), u32>,
    queue: VecDeque<(usize, RawRef, u32)>,
    /// The first output page made from each source page.
    pages: HashMap<(usize, usize), u32>,
    /// Every page comes from this source: its catalog is the output's.
    single: Option<usize>,
    /// Stamp images already written, by file.
    images: HashMap<PathBuf, u32>,
    /// Form fields with kids whose `/Kids` are pruned when copied (the
    /// output lacks some of the pages their widgets are on).
    form_nodes: HashSet<(usize, u32)>,
    /// Merged forms: fields renamed (new `/T`) so that equal names from
    /// different documents stay different fields.
    field_names: HashMap<(usize, u32), Vec<u8>>,
    /// Structure elements whose content is only on pages left out.
    dropped_elements: HashSet<(usize, u32)>,
    /// A reference could not get a number (too many objects).
    overflow: bool,
}

impl<'s> Writer<'s> {
    fn new(sources: &'s [Source], single: Option<usize>, version: &str) -> Self {
        let mut out = format!("%PDF-{version}\n%").into_bytes();
        // Binary bytes in the second line mark the file as binary.
        out.extend_from_slice(&[0xE2, 0xE3, 0xCF, 0xD3, b'\n']);
        Self {
            sources,
            out,
            offsets: Vec::new(),
            copied: HashMap::new(),
            queue: VecDeque::new(),
            pages: HashMap::new(),
            single,
            images: HashMap::new(),
            form_nodes: HashSet::new(),
            field_names: HashMap::new(),
            dropped_elements: HashSet::new(),
            overflow: false,
        }
    }

    fn alloc(&mut self) -> Result<u32, WriteError> {
        if self.offsets.len() >= MAX_OBJECTS {
            self.overflow = true;
            return Err(too_many_objects());
        }
        self.offsets.push(None);
        Ok(self.offsets.len() as u32)
    }

    fn object(&mut self, number: u32, body: &[u8]) {
        self.start(number);
        self.out.extend_from_slice(body);
        self.out.extend_from_slice(b"\nendobj\n");
    }

    /// `dict` must have the right `/Length`.
    fn stream(&mut self, number: u32, dict: &[u8], data: &[u8]) {
        self.start(number);
        self.out.extend_from_slice(dict);
        self.out.extend_from_slice(b"\nstream\n");
        self.out.extend_from_slice(data);
        self.out.extend_from_slice(b"\nendstream\nendobj\n");
    }

    fn start(&mut self, number: u32) {
        if let Some(slot) = self.offsets.get_mut(number as usize - 1) {
            *slot = Some(self.out.len());
        }
        self.out
            .extend_from_slice(format!("{number} 0 obj\n").as_bytes());
    }

    /// The output number for reference `r` of source `s`: `None` (written
    /// as `null`) when the object must not be copied.
    fn target(&mut self, s: usize, r: RawRef) -> Option<u32> {
        let source = &self.sources[s];
        let number = r.0;
        if let Some(&page) = source.page_of.get(&number) {
            return self.pages.get(&(s, page)).copied();
        }
        if source.tree.contains(&number) {
            return None;
        }
        if source
            .annotation_pages
            .get(&number)
            .is_some_and(|pages| !self.keeps_any(s, pages))
        {
            return None;
        }
        if self.dropped_elements.contains(&(s, number)) {
            return None;
        }
        if i64::from(number) == i64::from(source.catalog.obj_number) {
            return (self.single == Some(s)).then_some(CATALOG);
        }
        if let Some(&n) = self.copied.get(&(s, number)) {
            return Some(n);
        }
        let n = self.alloc().ok()?;
        self.copied.insert((s, number), n);
        self.queue.push_back((s, r, n));
        Some(n)
    }

    /// Whether the output has any of `pages` of source `s`.
    fn keeps_any(&self, s: usize, pages: &[usize]) -> bool {
        pages.iter().any(|&p| self.pages.contains_key(&(s, p)))
    }

    /// Whether the form field `r` (or a field below it) has a widget on a
    /// page the output keeps.
    fn field_kept(&self, s: usize, r: RawRef, depth: usize) -> bool {
        let source = &self.sources[s];
        if let Some(pages) = source.annotation_pages.get(&r.0) {
            return self.keeps_any(s, pages);
        }
        if depth > 32 {
            return false;
        }
        let Some(field) = source.pdf.xref().get::<Dict<'_>>(object_id(r)) else {
            return false;
        };
        let kids = self.resolve_array(s, syntax::dict_value(field.data(), b"Kids"));
        syntax::array_elements(&kids)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| syntax::single_reference(&kids[e]))
            .any(|kid| self.field_kept(s, kid, depth + 1))
    }

    /// An array value (direct, or a reference to one) as array bytes.
    fn resolve_array(&self, s: usize, raw: Option<&[u8]>) -> Vec<u8> {
        let Some(raw) = raw else {
            return b"[]".to_vec();
        };
        match syntax::single_reference(raw) {
            Some(r) => match self.sources[s].pdf.xref().get::<Object<'_>>(object_id(r)) {
                Some(Object::Array(a)) => a.data().to_vec(),
                _ => b"[]".to_vec(),
            },
            None if syntax::array_elements(raw).is_some() => raw.to_vec(),
            None => b"[]".to_vec(),
        }
    }

    /// The catalog's `/AcroForm` for an output without some of the source's
    /// pages: `/Fields` and `/CO` keep the fields that still have a widget;
    /// `None` when no field is left.
    fn pruned_form(&mut self, s: usize, form: &[u8]) -> Option<Vec<u8>> {
        let form = self.resolve_dict(s, Some(form));
        let fields = self.resolve_array(s, syntax::dict_value(&form, b"Fields"));
        // Every field with kids, at any depth: their kids are pruned too.
        for item in references_in(&fields) {
            self.collect_form_nodes(s, item, 0);
        }
        let fields = self.kept_fields(s, &fields);
        if fields == b"[]" {
            return None;
        }
        // Added values are written as they are: renumber them here.
        let mut add: Vec<(&[u8], Vec<u8>)> = vec![(b"Fields", self.remap(s, &fields))];
        if syntax::dict_value(&form, b"CO").is_some() {
            let order = self.resolve_array(s, syntax::dict_value(&form, b"CO"));
            let order = self.kept_fields(s, &order);
            add.push((b"CO", self.remap(s, &order)));
        }
        syntax::rebuild_dict(&form, &[], &add, |v| self.remap(s, v))
    }

    /// Marks the structure elements of source `s` whose content is only on
    /// pages the output does not have (`dropped_elements`).
    fn prune_structure(&mut self, s: usize, root: &[u8]) {
        let root = self.resolve_dict(s, Some(root));
        for element in self.kids(s, syntax::dict_value(&root, b"K")) {
            if let Some(r) = syntax::single_reference(&element) {
                self.walk_element(s, r, None, 0);
            }
        }
    }

    /// Whether structure element `r` keeps any content (on a kept page, or
    /// of unknown page); marks it dropped when all its content is gone.
    fn walk_element(&mut self, s: usize, r: RawRef, page: Option<u32>, depth: usize) -> bool {
        if depth > 128 {
            return true;
        }
        let sources = self.sources;
        let Some(element) = sources[s].pdf.xref().get::<Dict<'_>>(object_id(r)) else {
            return true;
        };
        let data = element.data();
        let page = syntax::dict_value(data, b"Pg")
            .and_then(syntax::single_reference)
            .map(|p| p.0)
            .or(page);
        let on_kept = |this: &Self, page: Option<u32>| match page {
            Some(number) => match sources[s].page_of.get(&number) {
                Some(&index) => this.pages.contains_key(&(s, index)),
                None => true,
            },
            None => true,
        };
        let mut attributable = false;
        let mut kept = false;
        for item in self.kids(s, syntax::dict_value(data, b"K")) {
            attributable = true;
            if let Some(child) = syntax::single_reference(&item) {
                let Some(dict) = sources[s].pdf.xref().get::<Dict<'_>>(object_id(child)) else {
                    continue;
                };
                if is_content_reference(dict.data()) {
                    let pg = syntax::dict_value(dict.data(), b"Pg")
                        .and_then(syntax::single_reference)
                        .map(|p| p.0)
                        .or(page);
                    kept |= on_kept(self, pg);
                } else {
                    kept |= self.walk_element(s, child, page, depth + 1);
                }
            } else if syntax::dict_entries(&item).is_some() {
                let pg = syntax::dict_value(&item, b"Pg")
                    .and_then(syntax::single_reference)
                    .map(|p| p.0)
                    .or(page);
                kept |= on_kept(self, pg);
            } else {
                // A marked-content id on the element's page.
                kept |= on_kept(self, page);
            }
        }
        if attributable && !kept {
            self.dropped_elements.insert((s, r.0));
        }
        kept || !attributable
    }

    /// The items of a structure `/K` value: one item (a marked-content id, a
    /// dictionary or a reference) or an array of them, maybe behind a
    /// reference.
    fn kids(&self, s: usize, raw: Option<&[u8]>) -> Vec<Vec<u8>> {
        let Some(raw) = raw else {
            return Vec::new();
        };
        let elements = |data: &[u8]| -> Option<Vec<Vec<u8>>> {
            Some(
                syntax::array_elements(data)?
                    .into_iter()
                    .map(|r| data[r].to_vec())
                    .collect(),
            )
        };
        if let Some(r) = syntax::single_reference(raw)
            && let Some(Object::Array(array)) =
                self.sources[s].pdf.xref().get::<Object<'_>>(object_id(r))
        {
            return elements(array.data()).unwrap_or_default();
        }
        elements(raw).unwrap_or_else(|| vec![raw.to_vec()])
    }

    /// A merged document's form: the fields with a widget on a kept page,
    /// from every source with a form; names that two documents share get a
    /// suffix so they stay separate fields. Default resources and
    /// appearance come from the first form. `None` without fields.
    fn merged_form(&mut self) -> Option<Vec<u8>> {
        let mut used: Vec<usize> = self.pages.keys().map(|&(s, _)| s).collect();
        used.sort_unstable();
        used.dedup();
        let mut fields: Vec<Vec<u8>> = Vec::new();
        let mut names: HashSet<Vec<u8>> = HashSet::new();
        let mut defaults: Vec<(&[u8], Vec<u8>)> = Vec::new();
        let mut need_appearances = false;
        for s in used {
            let sources = self.sources;
            let Some(catalog) = sources[s].pdf.xref().get::<Dict<'_>>(sources[s].catalog) else {
                continue;
            };
            let Some(raw) = syntax::dict_value(catalog.data(), b"AcroForm") else {
                continue;
            };
            let form = self.resolve_dict(s, Some(raw));
            let list = self.resolve_array(s, syntax::dict_value(&form, b"Fields"));
            for item in references_in(&list) {
                self.collect_form_nodes(s, item, 0);
            }
            for r in references_in(&self.kept_fields(s, &list)) {
                let name = sources[s]
                    .pdf
                    .xref()
                    .get::<Dict<'_>>(object_id(r))
                    .and_then(|f| f.get::<hayro::hayro_syntax::object::String<'_>>(b"T"))
                    .map(|t| t.as_bytes().to_vec())
                    .unwrap_or_default();
                let mut unique = name.clone();
                let mut n = 2;
                while !name.is_empty() && !names.insert(unique.clone()) {
                    unique = suffixed(&name, n);
                    n += 1;
                }
                if unique != name {
                    self.field_names.insert((s, r.0), unique);
                }
                fields.push(self.remap(s, format!("{} {} R", r.0, r.1).as_bytes()));
            }
            need_appearances |= syntax::dict_value(&form, b"NeedAppearances")
                .is_some_and(|v| v.trim_ascii() == b"true");
            if defaults.is_empty() {
                for key in [&b"DR"[..], b"DA", b"Q"] {
                    if let Some(value) = syntax::dict_value(&form, key) {
                        defaults.push((key, self.remap(s, value)));
                    }
                }
            }
        }
        if fields.is_empty() {
            return None;
        }
        let mut out = b"<</Fields [".to_vec();
        out.extend_from_slice(&fields.join(&b' '));
        out.push(b']');
        for (key, value) in defaults {
            syntax::encode_name(key, &mut out);
            out.push(b' ');
            out.extend_from_slice(&value);
        }
        if need_appearances {
            out.extend_from_slice(b"/NeedAppearances true");
        }
        out.extend_from_slice(b">>");
        Some(out)
    }

    /// The references of `array` (source numbering) to fields that still
    /// have a widget on a kept page, as an array.
    fn kept_fields(&self, s: usize, array: &[u8]) -> Vec<u8> {
        let kept: Vec<String> = references_in(array)
            .into_iter()
            .filter(|&r| self.field_kept(s, r, 0))
            .map(|(n, g)| format!("{n} {g} R"))
            .collect();
        format!("[{}]", kept.join(" ")).into_bytes()
    }

    fn collect_form_nodes(&mut self, s: usize, r: RawRef, depth: usize) {
        if depth > 32 || self.sources[s].annotation_pages.contains_key(&r.0) {
            return;
        }
        let Some(field) = self.sources[s].pdf.xref().get::<Dict<'_>>(object_id(r)) else {
            return;
        };
        let kids = self.resolve_array(s, syntax::dict_value(field.data(), b"Kids"));
        let kids = references_in(&kids);
        if kids.is_empty() || !self.form_nodes.insert((s, r.0)) {
            return;
        }
        for kid in kids {
            self.collect_form_nodes(s, kid, depth + 1);
        }
    }

    /// A merged document's optional content: every source's layers, with
    /// the layers its default configuration hides still hidden. `None`
    /// when no source has layers.
    fn merged_layers(&mut self) -> Option<Vec<u8>> {
        let mut used: Vec<usize> = self.pages.keys().map(|&(s, _)| s).collect();
        used.sort_unstable();
        used.dedup();
        let (mut groups, mut hidden, mut order) = (Vec::new(), Vec::new(), Vec::new());
        for s in used {
            let sources = self.sources;
            let Some(catalog) = sources[s].pdf.xref().get::<Dict<'_>>(sources[s].catalog) else {
                continue;
            };
            let Some(raw) = syntax::dict_value(catalog.data(), b"OCProperties") else {
                continue;
            };
            let properties = self.resolve_dict(s, Some(raw));
            let config = self.resolve_dict(s, syntax::dict_value(&properties, b"D"));
            let refs = |this: &Self, dict: &[u8], key: &[u8]| -> Vec<RawRef> {
                references_in(&this.resolve_array(s, syntax::dict_value(dict, key)))
            };
            let all = refs(self, &properties, b"OCGs");
            // With `/BaseState /OFF`, every layer not listed in `/ON` is
            // hidden; otherwise the ones listed in `/OFF`.
            let base_off = syntax::dict_value(&config, b"BaseState")
                .is_some_and(|v| v.trim_ascii() == b"/OFF");
            let off: Vec<RawRef> = if base_off {
                let on: Vec<u32> = refs(self, &config, b"ON").iter().map(|r| r.0).collect();
                all.iter().copied().filter(|r| !on.contains(&r.0)).collect()
            } else {
                refs(self, &config, b"OFF")
            };
            for (list, out) in [(all, &mut groups), (off, &mut hidden)] {
                for (n, g) in list {
                    out.push(self.remap(s, format!("{n} {g} R").as_bytes()));
                }
            }
            let array = self.resolve_array(s, syntax::dict_value(&config, b"Order"));
            for element in syntax::array_elements(&array).unwrap_or_default() {
                order.push(self.remap(s, &array[element]));
            }
        }
        if groups.is_empty() {
            return None;
        }
        let list = |items: &[Vec<u8>]| [b"[".to_vec(), items.join(&b' '), b"]".to_vec()].concat();
        let mut out = b"<</OCGs ".to_vec();
        out.extend_from_slice(&list(&groups));
        out.extend_from_slice(b"/D <</BaseState /ON/OFF ");
        out.extend_from_slice(&list(&hidden));
        out.extend_from_slice(b"/Order ");
        out.extend_from_slice(&list(&order));
        out.extend_from_slice(b">>>>");
        Some(out)
    }

    fn remap(&mut self, s: usize, data: &[u8]) -> Vec<u8> {
        syntax::remap(data, |r| self.target(s, r))
    }

    /// Copies everything the written objects reach.
    fn drain(&mut self) -> Result<(), WriteError> {
        while let Some((s, r, n)) = self.queue.pop_front() {
            let sources = self.sources;
            let object = sources[s].pdf.xref().get::<Object<'_>>(object_id(r));
            let body = match object {
                Some(Object::Dict(d))
                    if self.form_nodes.contains(&(s, r.0))
                        || self.field_names.contains_key(&(s, r.0)) =>
                {
                    // A form field: its kids without the deleted pages'
                    // fields, and maybe a new name.
                    let mut add: Vec<(&[u8], Vec<u8>)> = Vec::new();
                    if self.form_nodes.contains(&(s, r.0)) {
                        let kids = self.resolve_array(s, syntax::dict_value(d.data(), b"Kids"));
                        let kids = self.kept_fields(s, &kids);
                        add.push((b"Kids", self.remap(s, &kids)));
                    }
                    if let Some(name) = self.field_names.get(&(s, r.0)) {
                        add.push((b"T", hex_string(name)));
                    }
                    syntax::rebuild_dict(d.data(), &[], &add, |v| self.remap(s, v))
                        .unwrap_or_else(|| self.remap(s, d.data()))
                }
                Some(Object::Dict(d)) => self.remap(s, d.data()),
                Some(Object::Array(a)) => self.remap(s, a.data()),
                Some(Object::Stream(stream)) => {
                    self.copy_stream(s, n, &stream)?;
                    continue;
                }
                Some(other) => primitive(&other),
                None => b"null".to_vec(),
            };
            self.object(n, &body);
        }
        if self.overflow {
            return Err(too_many_objects());
        }
        Ok(())
    }

    fn copy_stream(&mut self, s: usize, n: u32, stream: &Stream<'_>) -> Result<(), WriteError> {
        let data = stream.raw_data();
        let length = data.len().to_string().into_bytes();
        let dict = syntax::rebuild_dict(stream.dict().data(), &[], &[(b"Length", length)], |v| {
            self.remap(s, v)
        })
        .ok_or_else(|| unreadable(s, "a stream dictionary"))?;
        self.stream(n, &dict, &data);
        Ok(())
    }

    fn write_catalog(&mut self, keep_labels: bool) -> Result<(), WriteError> {
        let Some(s) = self.single else {
            // Pages of several documents: a catalog of its own, with their
            // layers so hidden ones stay hidden.
            let mut catalog = format!("<</Type /Catalog/Pages {PAGES_ROOT} 0 R").into_bytes();
            if let Some(layers) = self.merged_layers() {
                catalog.extend_from_slice(b"/OCProperties ");
                catalog.extend_from_slice(&layers);
            }
            if let Some(form) = self.merged_form() {
                catalog.extend_from_slice(b"/AcroForm ");
                catalog.extend_from_slice(&form);
            }
            catalog.extend_from_slice(b">>");
            self.object(CATALOG, &catalog);
            return Ok(());
        };
        let sources = self.sources;
        let catalog = sources[s]
            .pdf
            .xref()
            .get::<Dict<'_>>(sources[s].catalog)
            .ok_or_else(|| unreadable(s, "the catalog"))?;
        // `/Perms` (permissions of a signature) no longer applies.
        let mut drop: Vec<&[u8]> = vec![b"Perms"];
        if !keep_labels {
            drop.push(b"PageLabels");
        }
        let pages = format!("{PAGES_ROOT} 0 R").into_bytes();
        let mut add: Vec<(&[u8], Vec<u8>)> = vec![(b"Pages", pages)];
        // Without some of the pages, the form loses their fields.
        let every_page = (0..sources[s].page_count()).all(|p| self.pages.contains_key(&(s, p)));
        if !every_page && let Some(form) = syntax::dict_value(catalog.data(), b"AcroForm") {
            match self.pruned_form(s, form) {
                Some(form) => add.push((b"AcroForm", form)),
                None => drop.push(b"AcroForm"),
            }
        }
        if !every_page && let Some(root) = syntax::dict_value(catalog.data(), b"StructTreeRoot") {
            self.prune_structure(s, root);
        }
        let body = syntax::rebuild_dict(catalog.data(), &drop, &add, |v| self.remap(s, v))
            .ok_or_else(|| unreadable(s, "the catalog"))?;
        self.object(CATALOG, &body);
        Ok(())
    }

    fn write_page(&mut self, pick: &PagePick, number: u32) -> Result<(), WriteError> {
        let sources = self.sources;
        let s = pick.source;
        let page = &sources[s].pdf.pages()[pick.page.as_usize()];
        let dict = page.raw();
        let data = dict.data();
        let rotate = (hayro_degrees(page.rotation()) + pick.rotate.degrees()) % 360;
        let mut add: Vec<(&[u8], Vec<u8>)> = vec![
            (b"Parent", format!("{PAGES_ROOT} 0 R").into_bytes()),
            (b"Rotate", rotate.to_string().into_bytes()),
            (b"MediaBox", rect_array(page.media_box())),
            (b"CropBox", rect_array(page.crop_box())),
        ];
        // The page tree is flat now: inherited resources become the page's.
        let own = syntax::dict_value(data, b"Resources");
        let resources = own
            .map(<[u8]>::to_vec)
            .or_else(|| inherited(dict, b"Resources"));
        if !pick.stamps.is_empty() {
            let contents = syntax::dict_value(data, b"Contents");
            let (resources, contents) = self.stamp(pick, page, resources.as_deref(), contents)?;
            add.push((b"Resources", resources));
            add.push((b"Contents", contents));
        } else if own.is_none()
            && let Some(inherited) = resources
        {
            add.push((b"Resources", self.remap(s, &inherited)));
        }
        let body = syntax::rebuild_dict(data, &[], &add, |v| self.remap(s, v))
            .ok_or_else(|| unreadable(s, "a page dictionary"))?;
        self.object(number, &body);
        Ok(())
    }

    /// The page's resources with the stamp images added, and its contents
    /// wrapped in `q` / `Q` (so nothing the page leaves in the graphics
    /// state moves the stamps) followed by the stamps.
    fn stamp(
        &mut self,
        pick: &PagePick,
        page: &Page<'_>,
        resources: Option<&[u8]>,
        contents: Option<&[u8]>,
    ) -> Result<(Vec<u8>, Vec<u8>), WriteError> {
        let s = pick.source;
        let resources = self.resolve_dict(s, resources);
        let xobjects = self.resolve_dict(s, syntax::dict_value(&resources, b"XObject"));
        let taken: HashSet<Vec<u8>> = syntax::dict_entries(&xobjects)
            .unwrap_or_default()
            .into_iter()
            .map(|e| e.key)
            .collect();
        let geom = PageGeom::from_page(page);
        let mut names: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        let mut drawing = b"\nQ\n".to_vec();
        let mut k = 0;
        for stamp in &pick.stamps {
            let image = self.image(stamp)?;
            let name = loop {
                let name = format!("FastPdfStamp{k}");
                k += 1;
                if !taken.contains(name.as_bytes()) {
                    break name;
                }
            };
            drawing.extend_from_slice(
                format!("q {} cm /{name} Do Q\n", placement(&geom, stamp)).as_bytes(),
            );
            names.push((name.into_bytes(), format!("{image} 0 R").into_bytes()));
        }
        let add: Vec<(&[u8], Vec<u8>)> = names
            .iter()
            .map(|(name, value)| (name.as_slice(), value.clone()))
            .collect();
        let xobjects = syntax::rebuild_dict(&xobjects, &[], &add, |v| self.remap(s, v))
            .ok_or_else(|| unreadable(s, "an XObject dictionary"))?;
        let resources = syntax::rebuild_dict(&resources, &[], &[(b"XObject", xobjects)], |v| {
            self.remap(s, v)
        })
        .ok_or_else(|| unreadable(s, "a resource dictionary"))?;

        let open = self.alloc()?;
        self.stream(open, b"<</Length 2>>", b"q\n");
        let close = self.alloc()?;
        let close_dict = format!("<</Length {}>>", drawing.len());
        self.stream(close, close_dict.as_bytes(), &drawing);
        let mut items = vec![format!("{open} 0 R").into_bytes()];
        for item in self.content_items(s, contents) {
            items.push(self.remap(s, &item));
        }
        items.push(format!("{close} 0 R").into_bytes());
        let mut array = b"[".to_vec();
        array.extend_from_slice(&items.join(&b' '));
        array.push(b']');
        Ok((resources, array))
    }

    /// A dictionary value (direct, or a reference to one) as dictionary
    /// bytes; an empty dictionary when there is none.
    fn resolve_dict(&self, s: usize, raw: Option<&[u8]>) -> Vec<u8> {
        let Some(raw) = raw else {
            return b"<<>>".to_vec();
        };
        match syntax::single_reference(raw) {
            Some(r) => self.sources[s]
                .pdf
                .xref()
                .get::<Dict<'_>>(object_id(r))
                .map_or_else(|| b"<<>>".to_vec(), |d| d.data().to_vec()),
            None if syntax::dict_entries(raw).is_some() => raw.to_vec(),
            None => b"<<>>".to_vec(),
        }
    }

    /// The content streams of a `/Contents` value: one reference, an array
    /// of them, or a reference to such an array.
    fn content_items(&self, s: usize, contents: Option<&[u8]>) -> Vec<Vec<u8>> {
        let Some(raw) = contents else {
            return Vec::new();
        };
        let elements = |data: &[u8]| -> Vec<Vec<u8>> {
            syntax::array_elements(data)
                .unwrap_or_default()
                .into_iter()
                .map(|r| data[r].to_vec())
                .collect()
        };
        if let Some(r) = syntax::single_reference(raw) {
            return match self.sources[s].pdf.xref().get::<Object<'_>>(object_id(r)) {
                Some(Object::Array(array)) => elements(array.data()),
                _ => vec![raw.to_vec()],
            };
        }
        elements(raw)
    }

    /// The image XObject of a stamp, written once per file.
    fn image(&mut self, stamp: &Stamp) -> Result<u32, WriteError> {
        if let Some(&n) = self.images.get(&stamp.image) {
            return Ok(n);
        }
        let ImageObject {
            dict, data, smask, ..
        } = image::load(&stamp.image).map_err(|message| WriteError::Image {
            path: stamp.image.clone(),
            message,
        })?;
        let mask = match smask {
            Some((mask_dict, mask_data)) => {
                let n = self.alloc()?;
                let dict = format!("<<{mask_dict}/Length {}>>", mask_data.len());
                self.stream(n, dict.as_bytes(), &mask_data);
                Some(n)
            }
            None => None,
        };
        let n = self.alloc()?;
        let mut full = format!("<<{dict}/Length {}", data.len());
        if let Some(mask) = mask {
            full.push_str(&format!("/SMask {mask} 0 R"));
        }
        full.push_str(">>");
        self.stream(n, full.as_bytes(), &data);
        self.images.insert(stamp.image.clone(), n);
        Ok(n)
    }

    /// The document information of source `s` (title, author, ...).
    fn write_info(&mut self, s: usize) -> Result<Option<u32>, WriteError> {
        let m = self.sources[s].pdf.metadata();
        let fields: [(&str, &Option<Vec<u8>>); 6] = [
            ("Title", &m.title),
            ("Author", &m.author),
            ("Subject", &m.subject),
            ("Keywords", &m.keywords),
            ("Creator", &m.creator),
            ("Producer", &m.producer),
        ];
        let mut body = b"<<".to_vec();
        let mut any = false;
        for (key, value) in fields {
            if let Some(value) = value {
                body.extend_from_slice(format!("/{key} ").as_bytes());
                body.extend_from_slice(&hex_string(value));
                any = true;
            }
        }
        if !any {
            return Ok(None);
        }
        body.extend_from_slice(b">>");
        let n = self.alloc()?;
        self.object(n, &body);
        Ok(Some(n))
    }

    /// Cross-reference table and trailer. Checks that every object was
    /// written and starts where the table says.
    fn finish(mut self, info: Option<u32>) -> Result<Vec<u8>, WriteError> {
        let xref = self.out.len();
        let size = self.offsets.len() + 1;
        let mut table = format!("xref\n0 {size}\n0000000000 65535 f \n");
        for (i, offset) in self.offsets.iter().enumerate() {
            let number = i + 1;
            let ok = offset
                .is_some_and(|o| self.out[o..].starts_with(format!("{number} 0 obj\n").as_bytes()));
            let Some(offset) = offset.filter(|_| ok) else {
                return Err(WriteError::Job(format!(
                    "internal: object {number} was not written"
                )));
            };
            table.push_str(&format!("{offset:010} 00000 n \n"));
        }
        self.out.extend_from_slice(table.as_bytes());
        let info = info.map_or_else(String::new, |n| format!("/Info {n} 0 R"));
        self.out.extend_from_slice(
            format!(
                "trailer\n<</Size {size}/Root {CATALOG} 0 R{info}>>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        Ok(self.out)
    }
}

/// A value of `key` inherited from the page's ancestors.
fn inherited(page: &Dict<'_>, key: &[u8]) -> Option<Vec<u8>> {
    let mut node = page.get::<Dict<'_>>(b"Parent");
    for _ in 0..64 {
        let dict = node?;
        if let Some(value) = syntax::dict_value(dict.data(), key) {
            return Some(value.to_vec());
        }
        node = dict.get::<Dict<'_>>(b"Parent");
    }
    None
}

fn hayro_degrees(rotation: HayroRotation) -> u32 {
    match rotation {
        HayroRotation::None => 0,
        HayroRotation::Horizontal => 90,
        HayroRotation::Flipped => 180,
        HayroRotation::FlippedHorizontal => 270,
    }
}

/// The `cm` operands that draw a stamp's image (the unit square) into its
/// rectangle, turned clockwise by its rotation.
fn placement(geom: &PageGeom, stamp: &Stamp) -> String {
    let u = if geom.user_unit > 0.0 {
        geom.user_unit
    } else {
        1.0
    };
    let r = stamp.rect;
    let x = geom.crop_x0 + f64::from(r.x0) / u;
    let y = geom.crop_y0 + geom.base_h - f64::from(r.y1) / u;
    let w = f64::from(r.width()) / u;
    let h = f64::from(r.height()) / u;
    let m = match stamp.rotate {
        Rotation::R0 => [w, 0.0, 0.0, h, x, y],
        Rotation::R90 => [0.0, -h, w, 0.0, x, y + h],
        Rotation::R180 => [-w, 0.0, 0.0, -h, x + w, y + h],
        Rotation::R270 => [0.0, h, -w, 0.0, x + w, y],
    };
    m.iter().map(|&v| real(v)).collect::<Vec<_>>().join(" ")
}

fn rect_array(r: Rect) -> Vec<u8> {
    format!(
        "[{} {} {} {}]",
        real(r.x0),
        real(r.y0),
        real(r.x1),
        real(r.y1)
    )
    .into_bytes()
}

/// A number in PDF syntax: no exponent, at most 6 decimals.
fn real(v: f64) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    if v.fract() == 0.0 && v.abs() < 9.0e15 {
        return format!("{}", v as i64);
    }
    let text = format!("{v:.6}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" {
        "0".into()
    } else {
        text.to_owned()
    }
}

fn number(n: &Number) -> Vec<u8> {
    real(n.as_f64()).into_bytes()
}

fn hex_string(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() * 2 + 2);
    out.push(b'<');
    for b in bytes {
        out.extend_from_slice(format!("{b:02X}").as_bytes());
    }
    out.push(b'>');
    out
}

/// An indirect object that is neither a dictionary, an array nor a stream.
fn primitive(object: &Object<'_>) -> Vec<u8> {
    match object {
        Object::Boolean(true) => b"true".to_vec(),
        Object::Boolean(false) => b"false".to_vec(),
        Object::Number(n) => number(n),
        Object::String(s) => hex_string(s.as_bytes()),
        Object::Name(name) => {
            let mut out = Vec::new();
            syntax::encode_name(name, &mut out);
            out
        }
        Object::Null(_) | Object::Dict(_) | Object::Array(_) | Object::Stream(_) => {
            b"null".to_vec()
        }
    }
}

fn unreadable(source: usize, what: &str) -> WriteError {
    WriteError::Source {
        index: source,
        message: format!("cannot read {what}"),
    }
}

fn too_many_objects() -> WriteError {
    WriteError::Job(format!("more than {MAX_OBJECTS} objects"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_written_without_exponents() {
        assert_eq!(real(612.0), "612");
        assert_eq!(real(-0.5), "-0.5");
        assert_eq!(real(1e-7), "0");
        assert_eq!(real(0.1 + 0.2), "0.3");
        assert_eq!(real(f64::NAN), "0");
        assert_eq!(real(-1e-9), "0");
        assert_eq!(hex_string(b"A\x01"), b"<4101>");
    }
}
