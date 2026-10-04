//! Font workarounds of the adapter (`src/fonts.rs`): bullets for the unused
//! WinAnsiEncoding codes and the standard Times styles on Windows.

mod common;

use common::*;
use fastpdf_engine_api::{CancelToken, EngineDocument, PageIndex, Pixmap, Rotation};
use zpdf_font::system::{SubstituteHints, find_system_font};

/// A PDF whose pages share the fonts `fonts` (resource name, font dictionary)
/// and each draw `content`. Pages are 300 x 100 pt.
fn fonts_pdf(fonts: &[(&str, &str)], contents: &[&[u8]]) -> Vec<u8> {
    let font_base = 3; // 1 catalog, 2 page tree
    let page_base = font_base + fonts.len();
    let mut objects = vec![
        obj("<< /Type /Catalog /Pages 2 0 R >>"),
        obj(&format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            (0..contents.len())
                .map(|i| format!("{} 0 R", page_base + 2 * i))
                .collect::<Vec<_>>()
                .join(" "),
            contents.len()
        )),
    ];
    for (_, dict) in fonts {
        objects.push(obj(dict));
    }
    let resources = fonts
        .iter()
        .enumerate()
        .map(|(i, (name, _))| format!("/{name} {} 0 R", font_base + i))
        .collect::<Vec<_>>()
        .join(" ");
    for (i, content) in contents.iter().enumerate() {
        objects.push(obj(&format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 100] /Contents {} 0 R \
             /Resources << /Font << {resources} >> >> >>",
            page_base + 2 * i + 1
        )));
        objects.push(stream("", content));
    }
    pdf(&objects)
}

fn render(doc: &dyn EngineDocument, page: u32) -> Pixmap {
    render_full(doc, page, 2.0, Rotation::R0)
}

fn has_ink(pixmap: &Pixmap) -> bool {
    pixmap.data().iter().any(|&b| b < 128)
}

const HELVETICA: &str =
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>";

#[test]
fn unused_win_ansi_codes_render_as_bullets() {
    // ReportLab writes list bullets as code 0x7F (`\177`). ISO 32000-1 maps
    // all six codes Windows-1252 leaves undefined to `bullet`, like 0x95.
    let unused = b"BT /F1 40 Tf 10 30 Td (\\177) Tj 45 0 Td (\\201) Tj 45 0 Td (\\215) Tj \
                   45 0 Td (\\217) Tj 45 0 Td (\\220) Tj 45 0 Td (\\235) Tj ET";
    let bullets = b"BT /F1 40 Tf 10 30 Td (\\225) Tj 45 0 Td (\\225) Tj 45 0 Td (\\225) Tj \
                    45 0 Td (\\225) Tj 45 0 Td (\\225) Tj 45 0 Td (\\225) Tj ET";
    let doc = open(fonts_pdf(&[("F1", HELVETICA)], &[unused, bullets])).expect("open");
    let (shown, expected) = (render(&doc, 0), render(&doc, 1));
    assert!(has_ink(&expected), "the reference bullets are missing");
    assert!(
        shown.data() == expected.data(),
        "unused WinAnsi codes are not drawn as bullets"
    );
    // Text extraction follows the same encoding.
    let text = doc
        .text_layer(PageIndex::FIRST, &CancelToken::new())
        .expect("text")
        .plain_text();
    assert_eq!(text.matches('\u{2022}').count(), 6, "{text:?}");
}

#[test]
fn bullets_are_found_in_hex_strings_too() {
    let hex = b"BT /F1 40 Tf 10 30 Td <7F> Tj ET";
    let literal = b"BT /F1 40 Tf 10 30 Td (\\225) Tj ET";
    let doc = open(fonts_pdf(&[("F1", HELVETICA)], &[hex, literal])).expect("open");
    assert!(render(&doc, 0).data() == render(&doc, 1).data());
}

#[test]
fn differences_still_override_the_bullets() {
    let custom = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding \
                  << /Type /Encoding /BaseEncoding /WinAnsiEncoding /Differences [127 /A] >> >>";
    let mapped = b"BT /F1 40 Tf 10 30 Td (\\177) Tj ET";
    let letter = b"BT /F1 40 Tf 10 30 Td (A) Tj ET";
    let doc = open(fonts_pdf(&[("F1", custom)], &[mapped, letter])).expect("open");
    let (shown, expected) = (render(&doc, 0), render(&doc, 1));
    assert!(has_ink(&expected));
    assert!(
        shown.data() == expected.data(),
        "/Differences entry ignored"
    );
}

#[test]
fn other_encodings_are_left_alone() {
    // StandardEncoding has no bullet rule: code 0x7F stays undefined, so
    // whatever zpdf draws for it must not be the bullet.
    let standard = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                    /Encoding /StandardEncoding >>";
    let unused = b"BT /F1 40 Tf 10 30 Td (\\177) Tj ET";
    let bullet = b"BT /F2 40 Tf 10 30 Td (\\225) Tj ET";
    let doc = open(fonts_pdf(
        &[("F1", standard), ("F2", HELVETICA)],
        &[unused, bullet],
    ))
    .expect("open");
    assert!(render(&doc, 0).data() != render(&doc, 1).data());
}

#[test]
fn standard_times_styles_render_with_their_own_faces() {
    let find = |name: &str| find_system_font(name, SubstituteHints::default(), None);
    let installed = ["Times-Roman", "TimesNewRoman,Bold", "TimesNewRoman,Italic"]
        .iter()
        .all(|name| find(name).is_some());
    if !installed {
        eprintln!("SKIPPED: Times New Roman faces are not installed");
        return;
    }
    // Equal explicit widths put every glyph at the same position, so the
    // pages differ only by the faces the fonts resolve to.
    let font = |base: &str| {
        format!(
            "<< /Type /Font /Subtype /Type1 /BaseFont /{base} /Encoding /WinAnsiEncoding \
             /FirstChar 32 /LastChar 126 /Widths [{}] >>",
            vec!["600"; 95].join(" ")
        )
    };
    let fonts = [
        font("Times-Roman"),
        font("Times-Bold"),
        font("Times-Italic"),
        font("Times-BoldItalic"),
    ];
    let names = ["F1", "F2", "F3", "F4"];
    let font_refs: Vec<(&str, &str)> = names
        .iter()
        .zip(&fonts)
        .map(|(n, f)| (*n, f.as_str()))
        .collect();
    let contents: Vec<Vec<u8>> = names
        .iter()
        .map(|n| format!("BT /{n} 30 Tf 10 30 Td (Hamburg) Tj ET").into_bytes())
        .collect();
    let contents: Vec<&[u8]> = contents.iter().map(Vec::as_slice).collect();
    let doc = open(fonts_pdf(&font_refs, &contents)).expect("open");
    let pages: Vec<Pixmap> = (0..4).map(|p| render(&doc, p)).collect();
    for (i, style) in ["Bold", "Italic", "BoldItalic"].iter().enumerate() {
        assert!(
            pages[i + 1].data() != pages[0].data(),
            "Times-{style} rendered with the regular face"
        );
    }
    assert!(pages[1].data() != pages[2].data());
    assert!(pages[2].data() != pages[3].data());
}
