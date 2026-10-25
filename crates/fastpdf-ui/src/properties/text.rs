//! Values of the properties panel as text: sizes, the page size with its
//! paper name, dates in the offset they were written with, encryption.
//! Document text is hostile (spec §24): what is shown goes through
//! [`display_value`], what is copied through [`copy_value`].

use fastpdf_engine_api::display::{clean_display, display_char};
use fastpdf_engine_api::{DocumentMetadata, PageSize, PdfDate, Security};

use crate::i18n::Strings;

/// Characters of one value shown at most; the copy keeps more.
pub(crate) const MAX_DISPLAY_CHARS: usize = 2000;
/// Bytes of one copied value at most (decoded strings are at most 16 KiB
/// of the file's bytes, a few times that as text).
pub(crate) const MAX_COPY_BYTES: usize = 64 * 1024;
/// Shown for a value the document does not have.
pub(crate) const MISSING: &str = "\u{2014}";
/// Shown for a value that is still being read.
pub(crate) const PENDING: &str = "\u{2026}";

/// `text` made safe to show: controls become spaces, bidi and zero-width
/// characters are removed, at most [`MAX_DISPLAY_CHARS`] characters
/// followed by an ellipsis when cut.
pub(crate) fn display_value(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(4 * MAX_DISPLAY_CHARS));
    for (count, c) in text.chars().filter_map(display_char).enumerate() {
        if count == MAX_DISPLAY_CHARS {
            out.push('\u{2026}');
            break;
        }
        out.push(c);
    }
    out
}

/// `text` as copied: cleaned like the display, but only cut past
/// [`MAX_COPY_BYTES`].
pub(crate) fn copy_value(text: &str) -> String {
    clean_display(text, MAX_COPY_BYTES)
}

/// `n` with thousands separators: "1,289,331".
pub(crate) fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "1.23 MB (1,289,331 bytes)" (units of 1024, as Windows shows them);
/// below 1 KB the byte count alone.
pub(crate) fn file_size(bytes: u64, strings: &Strings) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    let count = thousands(bytes);
    if bytes < 1024 {
        return strings.bytes(&count);
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    strings.size_with_bytes(&format!("{value:.2} {}", UNITS[unit]), &count)
}

/// Paper sizes in points (portrait), named when a page is within a point
/// of one in either orientation.
const PAPERS: [(&str, f32, f32); 6] = [
    ("A3", 841.89, 1190.55),
    ("A4", 595.28, 841.89),
    ("A5", 419.53, 595.28),
    ("B5 (JIS)", 515.91, 728.50),
    ("Letter", 612.0, 792.0),
    ("Legal", 612.0, 1008.0),
];

pub(crate) fn paper_name(size: PageSize) -> Option<&'static str> {
    let close = |a: f32, b: f32| (a - b).abs() <= 1.0;
    PAPERS.iter().find_map(|&(name, w, h)| {
        ((close(size.width, w) && close(size.height, h))
            || (close(size.width, h) && close(size.height, w)))
        .then_some(name)
    })
}

/// "210.0 × 297.0 mm (595.3 × 841.9 pt) · A4 · page 3" for page `page`
/// (one-based) of `size` points, unrotated.
pub(crate) fn page_size(size: PageSize, page: u32, strings: &Strings) -> String {
    let mm = |pt: f32| f64::from(pt) * 25.4 / 72.0;
    let mm_text = format!("{:.1} \u{00D7} {:.1} mm", mm(size.width), mm(size.height));
    let pt_text = format!("{:.1} \u{00D7} {:.1} pt", size.width, size.height);
    strings.page_size_text(&mm_text, &pt_text, paper_name(size), page)
}

/// A PDF date in the offset it was written with; the text as written
/// (made safe) when it is not a date.
pub(crate) fn date(raw: &str, strings: &Strings) -> String {
    let Some(d) = PdfDate::parse(raw) else {
        return display_value(raw.trim());
    };
    let offset = d.utc_offset_minutes.map(|minutes| {
        if minutes == 0 {
            "UTC".to_owned()
        } else {
            let sign = if minutes < 0 { '-' } else { '+' };
            let abs = minutes.unsigned_abs();
            format!("UTC{sign}{:02}:{:02}", abs / 60, abs % 60)
        }
    });
    strings.date_text(
        (d.year, d.month, d.day),
        (d.hour, d.minute, d.second),
        offset.as_deref(),
    )
}

/// The encryption row: the method with the handler and revision, "No" for
/// a document that is not encrypted.
pub(crate) fn encryption(
    metadata: &DocumentMetadata,
    security: Option<&Security>,
    strings: &Strings,
) -> String {
    match security {
        Some(s) => {
            let handler = s
                .handler
                .as_deref()
                .map(|h| clean_display(h, Security::MAX_HANDLER_BYTES));
            format!(
                "{}{}",
                strings.encryption_method(s.method),
                strings.encryption_details(handler.as_deref(), s.revision)
            )
        }
        None if metadata.encrypted => strings.encryption_details_unknown().to_owned(),
        None => strings.encryption_none().to_owned(),
    }
}

/// A count that may be a lower bound: "≥ 20000" when the engine stopped
/// counting.
pub(crate) fn count(n: u32, at_least: bool) -> String {
    if at_least {
        format!("\u{2265} {n}")
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use fastpdf_engine_api::EncryptionMethod;

    use super::*;
    use crate::i18n::Language;

    fn en() -> &'static Strings {
        Language::English.strings()
    }

    fn zh() -> &'static Strings {
        Language::TraditionalChinese.strings()
    }

    #[test]
    fn file_sizes() {
        assert_eq!(file_size(0, zh()), "0 位元組");
        assert_eq!(file_size(0, en()), "0 bytes");
        assert_eq!(file_size(1023, en()), "1,023 bytes");
        assert_eq!(file_size(1_289_331, zh()), "1.23 MB（1,289,331 位元組）");
        assert_eq!(file_size(1_289_331, en()), "1.23 MB (1,289,331 bytes)");
        assert_eq!(file_size(3 << 30, en()), "3.00 GB (3,221,225,472 bytes)");
        assert_eq!(file_size(2048, zh()), "2.00 KB（2,048 位元組）");
        assert_eq!(thousands(u64::MAX), "18,446,744,073,709,551,615");
        assert_eq!(thousands(999), "999");
    }

    #[test]
    fn page_sizes_name_their_paper() {
        let a4 = PageSize::new(595.28, 841.89);
        assert_eq!(
            page_size(a4, 3, zh()),
            "210.0 \u{00D7} 297.0 mm（595.3 \u{00D7} 841.9 pt）・A4・第 3 頁"
        );
        assert_eq!(
            page_size(PageSize::new(841.89, 595.28), 1, en()),
            "297.0 \u{00D7} 210.0 mm (841.9 \u{00D7} 595.3 pt) · A4 · page 1"
        );
        assert_eq!(paper_name(PageSize::LETTER), Some("Letter"));
        assert_eq!(paper_name(PageSize::new(1008.0, 612.0)), Some("Legal"));
        assert_eq!(paper_name(PageSize::new(596.0, 842.5)), Some("A4"));
        assert_eq!(paper_name(PageSize::new(600.0, 842.0)), None);
        assert_eq!(
            page_size(PageSize::new(200.0, 100.0), 2, en()),
            "70.6 \u{00D7} 35.3 mm (200.0 \u{00D7} 100.0 pt) · page 2"
        );
    }

    #[test]
    fn dates_keep_their_offset() {
        assert_eq!(
            date("D:20261024153000+08'00'", zh()),
            "2026/10/24 15:30:00（UTC+08:00）"
        );
        assert_eq!(
            date("D:20261024153000+08'00'", en()),
            "2026-10-24 15:30:00 (UTC+08:00)"
        );
        assert_eq!(date("D:20261024153000Z", en()), "2026-10-24 15:30:00 (UTC)");
        assert_eq!(
            date("D:20261025091500-00'30'", en()),
            "2026-10-25 09:15:00 (UTC-00:30)"
        );
        assert_eq!(date("20200101", zh()), "2020/01/01 00:00:00");
        // Not a date: the text, made safe.
        assert_eq!(date("D:2026-13-45", en()), "D:2026-13-45");
        assert_eq!(date("yesterday\u{202E}\n", en()), "yesterday");
    }

    #[test]
    fn hostile_text_is_made_safe() {
        assert_eq!(display_value("a\u{202E}b\nc\0d\u{200B}e"), "ab c de");
        let long = "附".repeat(5000);
        let shown = display_value(&long);
        assert_eq!(shown.chars().count(), MAX_DISPLAY_CHARS + 1);
        assert!(shown.ends_with('\u{2026}'));
        assert_eq!(
            display_value(&"x".repeat(MAX_DISPLAY_CHARS)).len(),
            MAX_DISPLAY_CHARS
        );
        // The copy is not cut at the display length.
        assert_eq!(copy_value(&long).chars().count(), 5000);
        assert_eq!(copy_value("a\u{202E}b"), "ab");
        assert!(copy_value(&"x".repeat(100_000)).len() <= MAX_COPY_BYTES);
    }

    #[test]
    fn encryption_rows() {
        let plain = DocumentMetadata::default();
        assert_eq!(encryption(&plain, None, zh()), "否");
        let encrypted = DocumentMetadata {
            encrypted: true,
            ..DocumentMetadata::default()
        };
        assert_eq!(
            encryption(&encrypted, None, en()),
            "Encrypted (details unavailable)"
        );
        let security = Security {
            handler: Some("Standard\u{202E}".into()),
            method: EncryptionMethod::Aes128,
            version: Some(4),
            revision: Some(4),
            permissions: None,
            encrypt_metadata: true,
            user_password: false,
        };
        assert_eq!(
            encryption(&encrypted, Some(&security), zh()),
            "AES 128 位元（Standard 安全性處理常式，修訂版 4）"
        );
        assert_eq!(
            encryption(&encrypted, Some(&security), en()),
            "128-bit AES (Standard security handler, revision 4)"
        );
        assert_eq!(count(20_000, true), "\u{2265} 20000");
        assert_eq!(count(3, false), "3");
    }
}
