//! The rows of the properties panel, in sections, from what the reader
//! knows (file, pages, the current page's size) and the engine's answer.
//! Pure: the panel, Copy all and the dev script use the same rows.

use std::path::Path;

use fastpdf_engine_api::{DocumentProperties, PageIndex, PageSize, Permissions};

use super::text::{self, MISSING, PENDING};
use crate::i18n::{PropertyLabel as L, Strings};

/// One row: what it is and its value (not cleaned yet: document text is
/// cleaned when shown or copied).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Row {
    pub label: L,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Section {
    pub title: L,
    /// Shown above the rows (the permissions note).
    pub note: Option<&'static str>,
    pub rows: Vec<Row>,
}

/// The engine's answer as far as the rows are concerned.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Answer<'a> {
    /// Asked, not in yet.
    Pending,
    Ready(&'a DocumentProperties),
    /// No answer (the engine cannot tell, or failed).
    None,
}

/// What the reader knows about the open document.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Facts<'a> {
    pub path: &'a Path,
    pub file_bytes: u64,
    pub page_count: u32,
    pub page: PageIndex,
    /// The current page's size (unrotated, `/UserUnit` applied), once
    /// known.
    pub page_size: Option<PageSize>,
    /// Opened with a password the reader typed (ADR 0014).
    pub password: bool,
    pub answer: Answer<'a>,
}

pub(crate) fn sections(facts: &Facts<'_>, strings: &Strings) -> Vec<Section> {
    let ready = match facts.answer {
        Answer::Ready(p) => Some(p),
        _ => None,
    };
    // A value from the engine: "…" while it is being read, "—" without one.
    let engine = |value: Option<String>| match (facts.answer, value) {
        (Answer::Pending, _) => PENDING.to_owned(),
        (_, Some(v)) => v,
        (_, None) => MISSING.to_owned(),
    };
    let meta = ready.map(|p| &p.metadata);
    let text = |pick: fn(&fastpdf_engine_api::DocumentMetadata) -> &Option<String>| {
        engine(meta.and_then(|m| pick(m).clone()))
    };
    let date = |pick: fn(&fastpdf_engine_api::DocumentMetadata) -> &Option<String>| {
        engine(meta.and_then(|m| pick(m).as_deref().map(|d| text::date(d, strings))))
    };
    let row = |label, value| Row { label, value };

    let description = Section {
        title: L::Description,
        note: None,
        rows: vec![
            row(L::FileName, crate::opening::display_name(facts.path)),
            row(
                L::Location,
                facts
                    .path
                    .parent()
                    .map_or_else(|| MISSING.to_owned(), |p| p.display().to_string()),
            ),
            row(L::Title, text(|m| &m.title)),
            row(L::Author, text(|m| &m.author)),
            row(L::Subject, text(|m| &m.subject)),
            row(L::Keywords, text(|m| &m.keywords)),
            row(L::Created, date(|m| &m.creation_date)),
            row(L::Modified, date(|m| &m.modification_date)),
            row(L::Creator, text(|m| &m.creator)),
            row(L::Producer, text(|m| &m.producer)),
        ],
    };

    let page_size = facts.page_size.map_or_else(
        || MISSING.to_owned(),
        |size| text::page_size(size, facts.page.display_number(), strings),
    );
    let flag = |pick: fn(&DocumentProperties) -> bool| {
        engine(ready.map(|p| strings.yes_no(pick(p)).to_owned()))
    };
    let advanced = Section {
        title: L::Advanced,
        note: None,
        rows: vec![
            row(L::PdfVersion, text(|m| &m.pdf_version)),
            row(L::Pages, facts.page_count.to_string()),
            row(L::PageSize, page_size),
            row(L::FileSize, text::file_size(facts.file_bytes, strings)),
            row(L::Tagged, flag(|p| p.tagged)),
            row(L::FastWebView, flag(|p| p.linearized)),
        ],
    };

    let mut security = Section {
        title: L::Security,
        note: None,
        rows: vec![row(
            L::Encryption,
            engine(ready.map(|p| text::encryption(&p.metadata, p.security.as_ref(), strings))),
        )],
    };
    if let Some(p) = ready {
        if p.metadata.encrypted || p.security.is_some() || facts.password {
            security.rows.push(row(
                L::OpenedWith,
                strings.opened_with(facts.password).to_owned(),
            ));
        }
        if let Some(permissions) = p.security.as_ref().and_then(|s| s.permissions) {
            security.note = Some(strings.permissions_note());
            security
                .rows
                .extend(permission_rows(permissions, strings).map(|(l, v)| row(l, v)));
        }
    }

    let forms = ready.map(|p| p.forms);
    let forms = Section {
        title: L::Forms,
        note: None,
        rows: vec![
            row(
                L::FormFields,
                engine(forms.map(|f| text::count(f.fields, f.truncated))),
            ),
            row(
                L::SignatureFields,
                engine(forms.map(|f| {
                    strings.signature_count(
                        &text::count(f.signature_fields, f.truncated),
                        &text::count(f.signatures, f.truncated),
                    )
                })),
            ),
            row(
                L::Xfa,
                engine(forms.map(|f| strings.yes_no(f.xfa).to_owned())),
            ),
        ],
    };
    vec![description, advanced, security, forms]
}

/// What the owner password allows, row by row (signing follows filling in
/// forms).
fn permission_rows(
    p: Permissions,
    strings: &Strings,
) -> impl Iterator<Item = (L, String)> + use<'_> {
    [
        (L::Printing, strings.print_permission(p.printing())),
        (L::Changing, strings.allowed(p.modify())),
        (L::Assembly, strings.allowed(p.assemble())),
        (L::Copying, strings.allowed(p.copy())),
        (
            L::AccessibilityCopying,
            strings.allowed(p.copy_for_accessibility()),
        ),
        (L::Commenting, strings.allowed(p.annotate())),
        (L::FillingForms, strings.allowed(p.fill_forms())),
        (L::Signing, strings.allowed(p.fill_forms())),
    ]
    .into_iter()
    .map(|(l, v)| (l, v.to_owned()))
}

/// Every row as "label: value" lines under their section titles, values
/// cleaned for copying (Copy all, the dev script).
pub(crate) fn as_text(sections: &[Section], strings: &Strings) -> String {
    let mut out = String::new();
    for section in sections {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(strings.property_label(section.title));
        out.push('\n');
        if let Some(note) = section.note {
            out.push_str(note);
            out.push('\n');
        }
        for row in &section.rows {
            out.push_str(&strings.property_line(row.label, &text::copy_value(&row.value)));
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests;
