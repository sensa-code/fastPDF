//! One label dictionary of the number tree: `/S` (style), `/P` (prefix)
//! and `/St` (start). Each may be written directly or as a reference; a
//! referenced value is read once per object (remembered in the walk), as
//! the type it must be, and charged like every other reference: by size
//! when it is that type, a fixed share of the budget when it is not.

use fastpdf_engine_api::display::clean_display;
use fastpdf_engine_api::{PageLabelStyle, PageLabels};
use hayro::hayro_syntax::object::{Dict, MaybeRef, Name, ObjectIdentifier, String as PdfString};

use super::{Entry, MAX_INDIRECT_VALUES, Walk};
use crate::nav::decode_text_string;

/// Bytes of one `/P` string decoded at most: enough for
/// [`PageLabels::MAX_PREFIX_BYTES`] of text in any text-string encoding.
const MAX_PREFIX_SOURCE_BYTES: usize = 4 * PageLabels::MAX_PREFIX_BYTES;

impl<'p> Walk<'p> {
    /// The label a dictionary describes; `None` once the byte budget is
    /// spent while reading it.
    pub(super) fn entry(&mut self, dict: &Dict<'p>) -> Option<Entry> {
        let style = match dict.get_raw::<Name<'p>>(b"S") {
            Some(MaybeRef::NotRef(name)) => style_named(name.as_str()),
            Some(MaybeRef::Ref(r)) => self.indirect_style(r.into()),
            None => PageLabelStyle::None,
        };
        let start = match dict.get_raw::<i64>(b"St") {
            Some(MaybeRef::NotRef(st)) => start_from(st),
            Some(MaybeRef::Ref(r)) => self.indirect_start(r.into()),
            None => 1,
        };
        let prefix = match dict.get_raw::<PdfString<'p>>(b"P") {
            // Part of the dictionary's bytes, already charged.
            Some(MaybeRef::NotRef(text)) => prefix_text(&text),
            Some(MaybeRef::Ref(r)) => self.indirect_prefix(r.into())?,
            None => String::new(),
        };
        (self.bytes <= super::MAX_SCANNED_BYTES).then_some(Entry {
            style,
            prefix,
            start,
        })
    }

    /// An indirect `/S`; anything but a name means no numbers.
    fn indirect_style(&mut self, id: ObjectIdentifier) -> PageLabelStyle {
        if let Some(known) = self.styles.get(&id) {
            return *known;
        }
        if self.styles.len() >= MAX_INDIRECT_VALUES {
            return PageLabelStyle::None;
        }
        let style = match self.xref().get::<Name<'p>>(id) {
            Some(name) => {
                self.charge(name.len());
                style_named(name.as_str())
            }
            None => {
                self.reject();
                PageLabelStyle::None
            }
        };
        self.styles.insert(id, style);
        style
    }

    /// An indirect `/St`; anything but a number means 1.
    fn indirect_start(&mut self, id: ObjectIdentifier) -> u32 {
        if let Some(known) = self.starts.get(&id) {
            return *known;
        }
        if self.starts.len() >= MAX_INDIRECT_VALUES {
            return 1;
        }
        let start = match self.xref().get::<i64>(id) {
            Some(st) => start_from(st),
            None => {
                self.reject();
                1
            }
        };
        self.starts.insert(id, start);
        start
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

/// `/S` values; unknown styles keep the prefix and drop the number.
fn style_named(name: &str) -> PageLabelStyle {
    match name {
        "D" => PageLabelStyle::Decimal,
        "R" => PageLabelStyle::UpperRoman,
        "r" => PageLabelStyle::LowerRoman,
        "A" => PageLabelStyle::UpperLetters,
        "a" => PageLabelStyle::LowerLetters,
        _ => PageLabelStyle::None,
    }
}

/// `/St`, at least 1 and at most `u32::MAX`.
fn start_from(st: i64) -> u32 {
    st.clamp(1, i64::from(u32::MAX)) as u32
}

/// A `/P` string decoded (at most its first bytes) and made safe to show.
fn prefix_text(text: &PdfString<'_>) -> String {
    let raw = text.as_bytes();
    let raw = raw.get(..MAX_PREFIX_SOURCE_BYTES).unwrap_or(raw);
    clean_display(&decode_text_string(raw), PageLabels::MAX_PREFIX_BYTES)
}
