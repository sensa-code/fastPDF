//! Link annotations of a page (ISO 32000-2 §12.5.6.5), read with
//! hayro-syntax's object API: the clickable rectangle and its target
//! (`crate::dest`). Hidden links are left out; repeated references to one
//! annotation are read once; the annotations looked at, the links returned
//! and the URI bytes have caps. A malformed `/Annots` array is read up to
//! its first unreadable entry (`crate::items`).

use std::collections::HashSet;

use fastpdf_engine_api::{EngineError, Link, LinkTarget, PageIndex};
use hayro::hayro_syntax::object::{Array, Dict, MaybeRef, Name, Object, Rect as PdfRect};
use hayro::kurbo::Rect;

use crate::dest::DestResolver;
use crate::document::{DocInner, Generation};
use crate::geometry::PageGeom;
use crate::items::items;

/// Link annotations returned per page.
const MAX_LINKS: usize = 10_000;
/// `/Annots` entries looked at per page (widgets, notes and garbage
/// included).
const MAX_ANNOTS: usize = 2 * MAX_LINKS;
/// Annotation flags (`/F`, ISO 32000-2 §12.5.3) of annotations that are
/// not shown, so not clickable either.
const ANNOT_HIDDEN: i64 = 1 << 1;
const ANNOT_NO_VIEW: i64 = 1 << 5;

pub(crate) fn links(
    doc: &DocInner,
    generation: &Generation,
    page: PageIndex,
) -> Result<Vec<Link>, EngineError> {
    let pages = generation.pdf.pages();
    let p = pages
        .get(page.as_usize())
        .ok_or(EngineError::PageOutOfRange {
            page,
            page_count: doc.page_count(),
        })?;
    let mut out = Vec::new();
    let Some(annots) = p.raw().get::<Array<'_>>(b"Annots") else {
        return Ok(out);
    };
    let geom = PageGeom::from_page(p);
    let xref = generation.pdf.xref();
    let mut resolver = DestResolver::new(doc, generation);
    let mut seen = HashSet::new();
    for item in items(&annots).take(MAX_ANNOTS) {
        let annot = match item {
            MaybeRef::Ref(r) => {
                if !seen.insert(r) {
                    continue;
                }
                match xref.get::<Dict<'_>>(r.into()) {
                    Some(annot) => annot,
                    None => continue,
                }
            }
            MaybeRef::NotRef(Object::Dict(annot)) => annot,
            MaybeRef::NotRef(_) => continue,
        };
        if annot
            .get::<Name<'_>>(b"Subtype")
            .is_none_or(|s| s.as_str() != "Link")
            || hidden(&annot)
        {
            continue;
        }
        let Some(rect) = annot.get::<PdfRect>(b"Rect") else {
            continue;
        };
        out.push(Link {
            bounds: geom.page_rect(Rect::new(rect.x0, rect.y0, rect.x1, rect.y1)),
            target: resolver.target(&annot).unwrap_or(LinkTarget::Unsupported),
        });
        if out.len() >= MAX_LINKS {
            break;
        }
    }
    Ok(out)
}

/// Flagged Hidden or NoView: not shown, so not a link either.
fn hidden(annot: &Dict<'_>) -> bool {
    annot
        .get::<i64>(b"F")
        .is_some_and(|flags| flags & (ANNOT_HIDDEN | ANNOT_NO_VIEW) != 0)
}
