//! `fastpdf --licenses`: the licenses of FastPDF and of the third-party code
//! linked into it.
//!
//! The text is `THIRD_PARTY_NOTICES.txt` at the repository root, written by
//! `tools/license_report.py --notices` and kept current by CI. It is embedded
//! so that `fastpdf.exe` on its own, without the release zip and its
//! `licenses/` folder, still carries the notices its dependencies' licenses
//! require (MIT and BSD copyright notices, Apache-2.0 texts and NOTICE files).

use std::io::Write as _;

/// FastPDF's license and every linked crate's license files, each distinct text once.
pub(crate) const NOTICES: &str = include_str!("../../../THIRD_PARTY_NOTICES.txt");

/// Writes [`NOTICES`] to stdout.
pub(crate) fn print() {
    // Output piped into a pager that quits early is not worth a panic.
    let _ = std::io::stdout().lock().write_all(NOTICES.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::NOTICES;

    #[test]
    fn notices_cover_fastpdf_and_what_it_links() {
        assert!(NOTICES.starts_with("FastPDF license notices"));
        // FastPDF's own terms and both texts.
        assert!(NOTICES.contains("(MIT OR Apache-2.0)"));
        assert!(NOTICES.contains("=== F1: FastPDF, LICENSE-MIT ==="));
        assert!(NOTICES.contains("=== F2: FastPDF, LICENSE-APACHE ==="));
        // The engine, its NOTICE file, and the vendored GPUI crate (ADR 0011).
        assert!(NOTICES.contains("- hayro "));
        assert!(NOTICES.contains("NOTICE"));
        assert!(NOTICES.contains("- gpui_windows "));
        // License texts, not just names.
        assert!(NOTICES.contains("Permission is hereby granted, free of charge"));
        assert!(NOTICES.contains("TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION"));
    }

    #[test]
    fn every_referenced_text_is_present() {
        // Crate lines name texts as T<n>; each must have its block.
        let (crates, texts) = NOTICES
            .split_once("\nTexts\n-----\n")
            .expect("texts section");
        let mut referenced: Vec<&str> = crates
            .split(|c: char| !(c.is_ascii_alphanumeric()))
            .filter(|w| {
                w.len() > 1 && w.starts_with('T') && w[1..].bytes().all(|b| b.is_ascii_digit())
            })
            .collect();
        referenced.sort_unstable();
        referenced.dedup();
        assert!(!referenced.is_empty());
        for t in referenced {
            assert!(texts.contains(&format!("=== {t}: ")), "{t} has no text");
        }
    }
}
