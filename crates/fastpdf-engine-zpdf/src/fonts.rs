//! Page fonts with workarounds for two zpdf font bugs (both drafted as
//! upstream issues in `docs/upstream-issues/zpdf.md`):
//!
//! * **WinAnsi bullets.** zpdf's WinAnsiEncoding leaves the codes that
//!   Windows-1252 does not define (0x7F, 0x81, 0x8D, 0x8F, 0x90, 0x9D)
//!   unmapped, although ISO 32000-1 (Annex D.2, notes to the Latin character
//!   set table) maps every unused WinAnsi code above octal 40 to `bullet`.
//!   An unmapped code is then drawn as a raw glyph id, so ReportLab's list
//!   bullets (code 0x7F, octal `\177`) come out as glyph 127 of the
//!   substitute font: "ù" in Arial and Times New Roman. The affected fonts
//!   get those codes mapped to `bullet`.
//! * **Times styles on Windows.** `Times-Bold`, `Times-Italic` and
//!   `Times-BoldItalic` resolve to the regular Times New Roman, because the
//!   file name `times.ttf` is indexed as the family `times`, which matches
//!   before the styled alias is tried. The affected fonts are loaded under the
//!   equivalent name `TimesNewRoman,Bold` (and so on), which zpdf resolves to
//!   the right face and still maps to the standard Times metrics. This fix is
//!   enabled only where zpdf's own lookup is observed to lose the style.
//!
//! zpdf hands a page its fonts as shared immutable values, so a page that
//! needs a fix gets all its fonts loaded afresh, patched, and put into a new
//! `FontCache` under the same resource names. Pages that need no fix (the
//! common case) keep zpdf's shared fonts untouched. Fonts that form XObjects
//! and annotation appearances load themselves are not patched.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use zpdf_core::{PdfDict, PdfName, PdfObject};
use zpdf_document::PdfDocument;
use zpdf_document::font_loader::{load_single_font, load_single_font_dict};
use zpdf_document::page::PdfPage;
use zpdf_font::system::{SubstituteHints, SystemFontMatch, find_system_font};
use zpdf_font::{FontCache, LoadedFont, PdfFontType};

/// WinAnsiEncoding codes that Windows-1252 leaves undefined; ISO 32000-1
/// maps them to `bullet`.
const WIN_ANSI_UNUSED: [u8; 6] = [0x7F, 0x81, 0x8D, 0x8F, 0x90, 0x9D];

/// The same codes as octal escapes inside a literal string.
const WIN_ANSI_UNUSED_OCTAL: [&[u8; 3]; 6] = [b"177", b"201", b"215", b"217", b"220", b"235"];

/// Standard Times styles and the names zpdf resolves to the right face.
const TIMES_STYLES: [(&str, &str); 3] = [
    ("Times-Bold", "TimesNewRoman,Bold"),
    ("Times-Italic", "TimesNewRoman,Italic"),
    ("Times-BoldItalic", "TimesNewRoman,BoldItalic"),
];

/// What one font needs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Fix {
    /// Map the unused WinAnsi codes to `bullet`.
    bullets: bool,
    /// Load under this `/BaseFont` instead.
    base_font: Option<&'static str>,
}

impl Fix {
    fn any(self) -> bool {
        self.bullets || self.base_font.is_some()
    }
}

/// The fonts of `page`, whose content stream is `content`, with the fixes
/// above applied where needed.
pub(crate) fn page_fonts(doc: &PdfDocument, page: &PdfPage, content: &[u8]) -> FontCache {
    let fonts = doc.load_page_fonts(page);
    let shows_unused_codes = shows_unused_win_ansi_codes(content);
    let mut fixes: HashMap<&str, (PdfDict, Fix)> = HashMap::new();
    for (name, &font_ref) in &page.resources.fonts {
        let Some((_, font)) = fonts.get_by_name(name) else {
            continue;
        };
        let Some(dict) = resolve_dict(doc, &PdfObject::Ref(font_ref)) else {
            continue;
        };
        let fix = Fix {
            bullets: shows_unused_codes && lacks_win_ansi_bullets(doc, &dict, font),
            base_font: times_alias(font),
        };
        if fix.any() {
            fixes.insert(name.as_str(), (dict, fix));
        }
    }
    if fixes.is_empty() {
        return fonts;
    }
    drop(fonts);

    // Rebuild the page's font set the way zpdf's loader does: every resource
    // name, a placeholder where a font cannot be loaded or does not fit.
    let max_bytes = doc.file().limits().max_font_cache_bytes;
    let mut cache = FontCache::new();
    for (name, &font_ref) in &page.resources.fonts {
        let loaded = match fixes.get(name.as_str()) {
            Some((dict, fix)) => load_fixed(doc, dict, *fix),
            None => load_single_font(doc.file(), font_ref),
        };
        let font = loaded.unwrap_or_else(|_| LoadedFont::new_placeholder(name.clone()));
        if cache
            .try_insert_with_limit(name.clone(), font, max_bytes)
            .is_none()
        {
            let placeholder = LoadedFont::new_placeholder(name.clone());
            let _ = cache.try_insert_with_limit(name.clone(), placeholder, max_bytes);
        }
    }
    cache
}

fn load_fixed(doc: &PdfDocument, dict: &PdfDict, fix: Fix) -> zpdf_core::Result<LoadedFont> {
    let mut font = match fix.base_font {
        Some(base_font) => {
            let mut renamed = dict.clone();
            renamed.insert(
                PdfName::new("BaseFont"),
                PdfObject::Name(PdfName::new(base_font)),
            );
            load_single_font_dict(doc.file(), &renamed)?
        }
        None => load_single_font_dict(doc.file(), dict)?,
    };
    if fix.bullets {
        add_win_ansi_bullets(&mut font);
    }
    Ok(font)
}

/// Maps the unused WinAnsi codes that are still unmapped (a `/Differences`
/// entry wins) to `bullet`.
fn add_win_ansi_bullets(font: &mut LoadedFont) {
    if let Some(encoding) = font.encoding.as_mut() {
        for code in WIN_ANSI_UNUSED {
            if encoding.glyph_name(code).is_none() {
                encoding.apply_difference(code, "bullet");
            }
        }
    }
}

/// A simple font whose encoding is based on WinAnsiEncoding and leaves one
/// of the unused codes unmapped.
fn lacks_win_ansi_bullets(doc: &PdfDocument, dict: &PdfDict, font: &LoadedFont) -> bool {
    let simple = matches!(font.font_type, PdfFontType::Type1 | PdfFontType::TrueType);
    let Some(encoding) = font.encoding.as_ref() else {
        return false;
    };
    simple
        && is_win_ansi_based(doc, dict)
        && WIN_ANSI_UNUSED
            .iter()
            .any(|&code| encoding.glyph_name(code).is_none())
}

/// `/Encoding /WinAnsiEncoding`, or a dictionary with that `/BaseEncoding`.
fn is_win_ansi_based(doc: &PdfDocument, dict: &PdfDict) -> bool {
    match dict.get("Encoding") {
        Some(PdfObject::Name(name)) => name.as_str() == "WinAnsiEncoding",
        Some(encoding) => resolve_dict(doc, encoding)
            .is_some_and(|e| e.get_name("BaseEncoding").ok() == Some("WinAnsiEncoding")),
        None => false,
    }
}

fn resolve_dict(doc: &PdfDocument, object: &PdfObject) -> Option<PdfDict> {
    match object {
        PdfObject::Dict(dict) => Some(dict.clone()),
        PdfObject::Ref(id) => doc.file().resolve(*id).ok()?.as_dict().ok().cloned(),
        _ => None,
    }
}

/// The name to load a substituted standard Times style under, when zpdf's
/// own lookup would lose the style on this system.
fn times_alias(font: &LoadedFont) -> Option<&'static str> {
    if !font.is_substitute {
        return None;
    }
    let index = TIMES_STYLES
        .iter()
        .position(|(name, _)| *name == font.base_font)?;
    times_aliases()[index]
}

/// Per style: the alias, if zpdf resolves the standard name to the regular
/// face but the alias to a different one. Font discovery is process-wide
/// and static, so this is decided once.
fn times_aliases() -> &'static [Option<&'static str>; 3] {
    static ALIASES: OnceLock<[Option<&'static str>; 3]> = OnceLock::new();
    ALIASES.get_or_init(|| {
        let find = |name: &str| find_system_font(name, SubstituteHints::default(), None);
        let same = |a: &SystemFontMatch, b: &SystemFontMatch| {
            a.face_index == b.face_index && (Arc::ptr_eq(&a.data, &b.data) || a.data == b.data)
        };
        let regular = find("Times-Roman");
        TIMES_STYLES.map(|(name, alias)| {
            let regular = regular.as_ref()?;
            let styled = find(name)?;
            let fixed = find(alias)?;
            (same(&styled, regular) && !same(&fixed, regular)).then_some(alias)
        })
    })
}

/// Whether `content` may show an unused WinAnsi code: as a raw byte, as an
/// octal escape in a literal string, or inside a hex string. It errs on the
/// side of yes (inline image data, escaped backslashes); a false positive
/// only costs reloading the page's fonts.
fn shows_unused_win_ansi_codes(content: &[u8]) -> bool {
    let mut i = 0;
    while i < content.len() {
        let byte = content[i];
        if WIN_ANSI_UNUSED.contains(&byte) {
            return true;
        }
        match byte {
            b'\\' => {
                let escape = content.get(i + 1..i + 4);
                if escape.is_some_and(|e| WIN_ANSI_UNUSED_OCTAL.iter().any(|o| o[..] == *e)) {
                    return true;
                }
                i += 2;
                continue;
            }
            // A hex string: `<` that is not part of `<<`.
            b'<' if content.get(i + 1) == Some(&b'<') => {
                i += 2;
                continue;
            }
            b'<' => {
                let end = content[i + 1..]
                    .iter()
                    .position(|&b| b == b'>')
                    .map_or(content.len(), |p| i + 1 + p);
                if hex_string_shows_unused_codes(&content[i + 1..end]) {
                    return true;
                }
                i = end + 1;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    false
}

/// Decodes the digits of a hex string (whitespace ignored, an odd last digit
/// padded with 0) and looks for an unused WinAnsi code.
fn hex_string_shows_unused_codes(digits: &[u8]) -> bool {
    let mut nibbles = digits.iter().filter_map(|&b| (b as char).to_digit(16));
    while let Some(high) = nibbles.next() {
        let low = nibbles.next().unwrap_or(0);
        if WIN_ANSI_UNUSED.contains(&((high * 16 + low) as u8)) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_scan_finds_unused_codes_in_every_string_form() {
        assert!(shows_unused_win_ansi_codes(b"BT (\\177) Tj ET"));
        assert!(shows_unused_win_ansi_codes(b"BT (a\\235b) Tj ET"));
        assert!(shows_unused_win_ansi_codes(b"BT (\x7f) Tj ET"));
        assert!(shows_unused_win_ansi_codes(b"BT <41 7F> Tj ET"));
        assert!(shows_unused_win_ansi_codes(b"BT [<4142>-20<8d>] TJ ET"));
        assert!(shows_unused_win_ansi_codes(b"BT <9> Tj ET")); // 0x90
        assert!(!shows_unused_win_ansi_codes(
            b"/P <</MCID 0>> BDC BT (a\\225b\\(\\)) Tj <414243> Tj ET EMC"
        ));
        assert!(!shows_unused_win_ansi_codes(b"BT (\\\\) Tj (177) Tj ET"));
        assert!(!shows_unused_win_ansi_codes(b""));
    }
}
