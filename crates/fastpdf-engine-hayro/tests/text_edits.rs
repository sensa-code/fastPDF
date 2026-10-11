//! Text edits (ADR 0013): lines of text replaced, checked through the text
//! layer, the content and the rendering of what was written; and the edits
//! the writer refuses.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::path::{Path, PathBuf};

use common::{PageSpec, build, build_with, full_request, open, render};
use fastpdf_engine_api::write::{
    EditProblem, FontFace, OutputContent, OutputSpec, PagePick, TextEdit, WriteError, WriteJob,
};
use fastpdf_engine_api::{
    CancelToken, EngineDocument, GuardedDocument, PageIndex, PageRect, PixelFormat, Rotation,
    TextLayer, TextSpan,
};
use fastpdf_engine_hayro::write::execute;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fastpdf-edit-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn system_font(file: &str) -> Option<FontFace> {
    let path = PathBuf::from(r"C:\Windows\Fonts").join(file);
    if path.exists() {
        Some(FontFace { path, index: 0 })
    } else {
        eprintln!("skipped: {file} is not installed");
        None
    }
}

fn page(content: &str) -> PageSpec {
    PageSpec::new([0.0, 0.0, 612.0, 792.0], content)
}

fn layer(doc: &GuardedDocument, page: u32) -> TextLayer {
    doc.text_layer(PageIndex::new(page), &CancelToken::new())
        .unwrap()
}

fn texts(layer: &TextLayer) -> Vec<&str> {
    layer.spans.iter().map(|s| s.text.as_str()).collect()
}

fn span<'a>(layer: &'a TextLayer, text: &str) -> &'a TextSpan {
    layer
        .spans
        .iter()
        .find(|s| s.text == text)
        .unwrap_or_else(|| panic!("no span {text:?} in {:?}", texts(layer)))
}

fn edit(span: &TextSpan, new: &str, font: &FontFace) -> TextEdit {
    TextEdit {
        rect: span.bounds,
        old: span.text.clone(),
        new: new.to_owned(),
        font: font.clone(),
    }
}

/// Writes `source` with `edits` on its first page (and its other pages
/// unchanged) to a new file; returns the written bytes.
fn write(dir: &Path, source: &[u8], edits: Vec<TextEdit>) -> Result<Vec<u8>, WriteError> {
    let input = dir.join("in.pdf");
    std::fs::write(&input, source).unwrap();
    let out = dir.join("out.pdf");
    let pages = open(source.to_vec()).page_count();
    let mut picks: Vec<PagePick> = (0..pages)
        .map(|p| PagePick::new(0, PageIndex::new(p)))
        .collect();
    picks[0].edits = edits;
    let job = WriteJob {
        sources: vec![input],
        outputs: vec![OutputSpec {
            path: out.clone(),
            content: OutputContent::Pages(picks),
        }],
    };
    match execute(&job) {
        Ok(_) => Ok(std::fs::read(&out).unwrap()),
        Err(e) => {
            assert!(!out.exists(), "a failed edit writes nothing");
            Err(e)
        }
    }
}

/// The decoded content of the written document's `page`.
fn content_of(bytes: &[u8], page: usize) -> Vec<u8> {
    let pdf = hayro::hayro_syntax::Pdf::new(bytes.to_vec()).unwrap();
    pdf.pages()[page].page_stream().unwrap().to_vec()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn hex(text: &str) -> Vec<u8> {
    text.bytes()
        .flat_map(|b| format!("{b:02X}").into_bytes())
        .collect()
}

fn close(a: PageRect, b: PageRect) -> bool {
    let near = |x: f32, y: f32| (x - y).abs() < 0.05;
    near(a.x0, b.x0) && near(a.y0, b.y0) && near(a.x1, b.x1) && near(a.y1, b.y1)
}

/// The full render of `page` at 100% (RGBA bytes) and its width.
fn rendered(doc: &GuardedDocument, page: u32) -> (Vec<u8>, usize) {
    let req = full_request(doc, page, 1.0, Rotation::R0);
    let pm = render(doc, &req, PixelFormat::default()).unwrap();
    (pm.data().to_vec(), pm.size().width as usize)
}

/// The rows `rows` (pixels from the top) of a render.
fn band(render: &(Vec<u8>, usize), rows: std::ops::Range<usize>) -> &[u8] {
    let stride = render.1 * 4;
    &render.0[rows.start * stride..rows.end * stride]
}

#[test]
fn a_line_is_drawn_anew_from_its_first_change() {
    let Some(arial) = system_font("arial.ttf") else {
        return;
    };
    let dir = scratch("first-change");
    let source = build(&[page(
        "BT /F1 12 Tf 72 700 Td (Plaintiff: John Smith) Tj ET \
         BT /F1 12 Tf 72 640 Td (The next line stays.) Tj ET",
    )]);
    let before = open(source.clone());
    let old = layer(&before, 0);
    let line = span(&old, "Plaintiff: John Smith");
    let out = write(
        &dir,
        &source,
        vec![edit(line, "Plaintiff: Jane Doe", &arial)],
    )
    .unwrap();

    let after = open(out.clone());
    let new = layer(&after, 0);
    // The kept start of the line, untouched, and the rest drawn anew right
    // after it in the content: the text layer reads one line again.
    let edited = span(&new, "Plaintiff: Jane Doe");
    assert!((edited.bounds.x0 - line.bounds.x0).abs() < 0.05);
    assert!((edited.bounds.height() - line.bounds.height()).abs() < 0.05);
    let j = edited.char_rect(11);
    assert!(close(j, line.char_rect(11)), "the kept J stays: {j:?}");
    assert!(close(
        span(&new, "The next line stays.").bounds,
        span(&old, "The next line stays.").bounds
    ));
    // The removed text is gone from the file, not only from the page.
    let content = content_of(&out, 0);
    assert!(!contains(&content, b"John") && !contains(&content, &hex("ohn")));
    assert!(!contains(&out, b"John Smith"));
    // Everything else renders exactly as before.
    let (a, b) = (rendered(&before, 0), rendered(&after, 0));
    assert_eq!(band(&a, 0..80), band(&b, 0..80));
    assert_eq!(band(&a, 110..792), band(&b, 110..792));
    assert_ne!(
        band(&a, 80..100),
        band(&b, 80..100),
        "the edited line changed"
    );
}

#[test]
fn text_after_the_removed_glyphs_keeps_its_place() {
    let Some(arial) = system_font("arial.ttf") else {
        return;
    };
    let dir = scratch("keeps-place");
    // "Hello" and "World" are one TJ apart by a wide gap; "Again" goes on
    // from where "World" ends. Character and word spacing apply to all.
    let source = build(&[page(
        "BT /F1 12 Tf 1.5 Tc 4 Tw 72 700 Td [(Hello) -4000 (World)] TJ (Again and) Tj ET",
    )]);
    let before = open(source.clone());
    let old = layer(&before, 0);
    assert_eq!(texts(&old).len(), 2, "{:?}", texts(&old));
    let hello = old.spans[0].clone();
    let world = old.spans[1].clone();
    let out = write(&dir, &source, vec![edit(&hello, "Howdy", &arial)]).unwrap();
    let new = layer(&open(out), 0);
    let moved = new
        .spans
        .iter()
        .find(|s| s.text == world.text)
        .unwrap_or_else(|| panic!("{:?}", texts(&new)));
    assert!(close(moved.bounds, world.bounds), "{moved:?} vs {world:?}");
    assert!(texts(&new).contains(&"H"));
    assert!(texts(&new).contains(&"owdy"));

    // Removing a space (word spacing) and the words after it in the same
    // string: the rest of the line still keeps its place.
    let dir = scratch("keeps-place-2");
    let source = build(&[page(
        "BT /F1 12 Tf 1 Tc 6 Tw 72 700 Td (one two) Tj [-6000] TJ (far) Tj ET",
    )]);
    let old = layer(&open(source.clone()), 0);
    let line = old.spans[0].clone();
    let far = span(&old, "far").bounds;
    let out = write(&dir, &source, vec![edit(&line, "one", &arial)]).unwrap();
    let new = layer(&open(out), 0);
    assert!(close(span(&new, "far").bounds, far));
    assert!(texts(&new).contains(&"one"), "{:?}", texts(&new));
    assert!(!texts(&new).iter().any(|t| t.contains("two")));
}

#[test]
fn deleting_a_line_draws_nothing_new() {
    let Some(arial) = system_font("arial.ttf") else {
        return;
    };
    let dir = scratch("delete");
    let source = build(&[page(
        "BT /F1 12 Tf 72 700 Td (Delete me) Tj 0 -20 Td (Keep me) Tj ET",
    )]);
    let old = layer(&open(source.clone()), 0);
    let out = write(
        &dir,
        &source,
        vec![edit(span(&old, "Delete me"), "", &arial)],
    )
    .unwrap();
    let new = layer(&open(out.clone()), 0);
    assert_eq!(texts(&new), ["Keep me"]);
    assert!(close(new.spans[0].bounds, span(&old, "Keep me").bounds));
    assert!(
        !contains(&out, b"/Type0"),
        "no font is embedded for nothing"
    );
}

#[test]
fn scaled_and_bold_text_keep_their_size_and_look() {
    let Some(arial) = system_font("arial.ttf") else {
        return;
    };
    let dir = scratch("scaled-bold");
    // Drawn at twice the size through the CTM, filled and stroked (a
    // "fake bold" many producers use for CJK fonts without a bold face).
    let source = build(&[page(
        "q 2 0 0 2 0 0 cm 0.8 0 0 rg 0 0 0.8 RG 0.4 w \
         BT 2 Tr /F1 12 Tf 36 350 Td (Bold heading) Tj ET Q",
    )]);
    let old = layer(&open(source.clone()), 0);
    let heading = span(&old, "Bold heading");
    let out = write(&dir, &source, vec![edit(heading, "Bold title", &arial)]).unwrap();
    let new = layer(&open(out.clone()), 0);
    let rest = span(&new, " title");
    assert!((rest.bounds.height() - heading.bounds.height()).abs() < 0.05);
    let content = content_of(&out, 0);
    let text = String::from_utf8_lossy(&content);
    assert!(text.contains("0.8 0 0 rg"), "{text}");
    // In the scaled CTM the line is drawn in, as the old one was.
    assert!(text.contains("0 0 0.8 RG 0.4 w"), "{text}");
    assert!(text.contains(" 2 Tr"), "{text}");
}

#[test]
fn text_objects_that_change_the_ctm_get_new_text_after_the_content() {
    let Some(arial) = system_font("arial.ttf") else {
        return;
    };
    let dir = scratch("cm-in-bt");
    // `cm` inside BT ... ET (not allowed, but met): after its ET the CTM is
    // not the one the text was drawn with, so the new text goes last.
    let source = build(&[page(
        "BT 2 0 0 2 0 0 cm /F1 12 Tf 36 350 Td (Inside cm) Tj ET          BT /F1 12 Tf 72 600 Td (Below) Tj ET",
    )]);
    let old = layer(&open(source.clone()), 0);
    let line = span(&old, "Inside cm");
    let out = write(&dir, &source, vec![edit(line, "Inside it", &arial)]).unwrap();
    let new = layer(&open(out.clone()), 0);
    assert!(close(
        span(&new, "Below").bounds,
        span(&old, "Below").bounds
    ));
    let rest = new
        .spans
        .iter()
        .find(|s| s.text.ends_with("it"))
        .unwrap_or_else(|| panic!("{:?}", texts(&new)));
    assert!((rest.bounds.height() - line.bounds.height()).abs() < 0.05);
    let content = String::from_utf8_lossy(&content_of(&out, 0)).into_owned();
    assert!(
        content.starts_with(
            "q
"
        ),
        "{content}"
    );
}

#[test]
fn several_edits_share_one_font() {
    let Some(arial) = system_font("arial.ttf") else {
        return;
    };
    let dir = scratch("two-edits");
    let source = build(&[page(
        "BT /F1 12 Tf 72 700 Td (Alpha one) Tj 0 -20 Td (Beta two) Tj 0 -20 Td (Gamma) Tj ET",
    )]);
    let old = layer(&open(source.clone()), 0);
    let edits = vec![
        edit(span(&old, "Alpha one"), "Alpha 1", &arial),
        edit(span(&old, "Beta two"), "Beta 2", &arial),
    ];
    let out = write(&dir, &source, edits).unwrap();
    let new = layer(&open(out.clone()), 0);
    for text in ["Alpha ", "1", "Beta ", "2", "Gamma"] {
        assert!(
            texts(&new).iter().any(|t| t.trim() == text.trim()),
            "{text:?} in {:?}",
            texts(&new)
        );
    }
    let fonts = out.windows(15).filter(|w| w == b"/Subtype /Type0").count();
    assert_eq!(fonts, 1, "one Type0 font for both lines");
}

#[test]
fn cid_text_written_by_an_edit_can_be_edited_again() {
    let Some(kaiu) = system_font("kaiu.ttf") else {
        return;
    };
    let dir = scratch("cid");
    let source = build(&[page("BT /F1 12 Tf 72 700 Td (Plaintiff: John) Tj ET")]);
    let old = layer(&open(source.clone()), 0);
    let first = write(
        &dir,
        &source,
        vec![edit(
            span(&old, "Plaintiff: John"),
            "Plaintiff: 王小明",
            &kaiu,
        )],
    )
    .unwrap();
    let middle = layer(&open(first.clone()), 0);
    let cjk = middle
        .spans
        .iter()
        .find(|s| s.text.contains("王小明"))
        .unwrap_or_else(|| panic!("{:?}", texts(&middle)))
        .clone();
    // Now the line is Identity-H text of an embedded TrueType subset.
    let dir = scratch("cid-2");
    let new_text = cjk.text.replace("小", "大");
    let second = write(&dir, &first, vec![edit(&cjk, &new_text, &kaiu)]).unwrap();
    let after = layer(&open(second), 0);
    let joined: String = texts(&after).concat();
    assert!(
        joined.contains('王') && joined.contains("大明"),
        "{joined:?}"
    );
    assert!(!joined.contains('小'), "{joined:?}");
    let wang = |l: &TextLayer| {
        l.spans
            .iter()
            .find(|s| s.text.contains('王'))
            .map(|s| s.char_rect(s.text.chars().position(|c| c == '王').unwrap()))
            .unwrap()
    };
    assert!(close(wang(&after), wang(&middle)), "the kept glyph stays");
}

#[test]
fn edits_that_cannot_be_made_are_refused() {
    let Some(arial) = system_font("arial.ttf") else {
        return;
    };
    let refused = |name: &str, source: Vec<u8>, line: &str, new: &str, font: &FontFace| {
        let dir = scratch(name);
        let old = layer(&open(source.clone()), 0);
        let edit = edit(span(&old, line), new, font);
        match write(&dir, &source, vec![edit]) {
            Err(WriteError::Edit {
                source: 0,
                page,
                problem,
            }) => {
                assert_eq!(page, PageIndex::FIRST);
                problem
            }
            other => panic!("{name}: {other:?}"),
        }
    };
    // Text of a form XObject.
    let form = build_with(
        &[PageSpec {
            resources: "/XObject << /Fm1 6 0 R >>".into(),
            ..page("q /Fm1 Do Q")
        }],
        |b| {
            b.add_stream(
                "/Type /XObject /Subtype /Form /BBox [0 0 612 792] \
                 /Resources << /Font << /F1 3 0 R >> >>",
                b"BT /F1 12 Tf 72 700 Td (In a form) Tj ET".to_vec(),
            );
        },
    );
    assert_eq!(
        refused("form", form, "In a form", "Changed", &arial),
        EditProblem::InObject
    );
    // Invisible text (the recognized text over a scan).
    let ocr = build(&[page("BT 3 Tr /F1 12 Tf 72 700 Td (Hidden words) Tj ET")]);
    assert_eq!(
        refused("ocr", ocr, "Hidden words", "Shown", &arial),
        EditProblem::Invisible
    );
    // A text object that never ends, in content that restores more states
    // than it saves: no place for the new text is known to be in the
    // default state.
    let underflow = build(&[page("Q BT /F1 12 Tf 72 700 Td (Unbalanced) Tj")]);
    assert_eq!(
        refused("underflow", underflow, "Unbalanced", "Balanced", &arial),
        EditProblem::Unsupported
    );
    // Characters the font does not have.
    let plain = build(&[page("BT /F1 12 Tf 72 700 Td (Plain text) Tj ET")]);
    assert_eq!(
        refused(
            "missing",
            plain.clone(),
            "Plain text",
            "Plain \u{10FFFD}",
            &arial
        ),
        EditProblem::MissingGlyphs("\u{10FFFD}".into())
    );
    // A font file that is not there.
    let nowhere = FontFace {
        path: PathBuf::from(r"C:\nowhere\none.ttf"),
        index: 0,
    };
    assert!(matches!(
        refused("no-font", plain.clone(), "Plain text", "Other", &nowhere),
        EditProblem::Font(_)
    ));

    // A line that is not (or no longer) what the edit says.
    let dir = scratch("stale");
    let old = layer(&open(plain.clone()), 0);
    let mut stale = edit(span(&old, "Plain text"), "New", &arial);
    stale.old = "Other text".into();
    assert!(matches!(
        write(&dir, &plain, vec![stale]),
        Err(WriteError::Edit {
            problem: EditProblem::NotFound,
            ..
        })
    ));
    // Two edits of the same line.
    let line = edit(span(&old, "Plain text"), "First", &arial);
    let again = edit(span(&old, "Plain text"), "Second", &arial);
    assert!(matches!(
        write(&dir, &plain, vec![line, again]),
        Err(WriteError::Edit {
            problem: EditProblem::NotFound,
            ..
        })
    ));
}

/// Edits lines of every text document of the generated corpus: each edit
/// is written (and checked by the writer) or refused with a reason, never
/// a failed verification. Slow:
/// `cargo test -p fastpdf-engine-hayro --test text_edits -- --ignored --nocapture`.
#[test]
#[ignore = "edits many lines of the generated corpus"]
fn corpus_edits() {
    let Some(kaiu) = system_font("kaiu.ttf") else {
        return;
    };
    let Some(manifest) = common::fixture("manifest.json") else {
        return;
    };
    let manifest = String::from_utf8(manifest).unwrap();
    let dir = scratch("corpus");
    let mut failures = Vec::new();
    for path in manifest
        .split("\"path\": \"")
        .skip(1)
        .filter_map(|s| s.split('"').next())
        .filter(|p| {
            [
                "traditional-chinese/",
                "japanese/",
                "cjk/",
                "fonts/",
                "small-text/",
                "large-text/",
            ]
            .iter()
            .any(|c| p.starts_with(c))
        })
    {
        let Some(bytes) = common::fixture(path) else {
            continue;
        };
        let doc = open(bytes.clone());
        let input = dir.join("in.pdf");
        std::fs::write(&input, &bytes).unwrap();
        let mut tally: std::collections::BTreeMap<String, usize> = Default::default();
        for p in 0..doc.page_count().min(3) {
            let spans = layer(&doc, p).spans;
            // Up to six lines spread over the page.
            let step = (spans.len() / 6).max(1);
            for span in spans.iter().step_by(step).take(6) {
                let mut new: String = span.text.trim_end().to_owned();
                new.pop();
                new.push('改');
                let mut pick = PagePick::new(0, PageIndex::new(p));
                pick.edits = vec![edit(span, &new, &kaiu)];
                let out = dir.join("out.pdf");
                let job = WriteJob {
                    sources: vec![input.clone()],
                    outputs: vec![OutputSpec {
                        path: out.clone(),
                        content: OutputContent::Pages(vec![pick]),
                    }],
                };
                let outcome = match execute(&job) {
                    Ok(_) => {
                        let text: String =
                            texts(&layer(&open(std::fs::read(&out).unwrap()), 0)).concat();
                        if !text.contains('改') {
                            failures.push(format!(
                                "{path} p{}: {:?} lost the new text",
                                p + 1,
                                span.text
                            ));
                        }
                        "written".to_owned()
                    }
                    Err(WriteError::Edit { problem, .. }) => match problem {
                        EditProblem::MissingGlyphs(_) => "missing glyphs".to_owned(),
                        EditProblem::Font(m) => format!("font: {m}"),
                        other => format!("{other:?}"),
                    },
                    Err(e) => {
                        failures.push(format!("{path} p{}: {:?}: {e}", p + 1, span.text));
                        "error".to_owned()
                    }
                };
                *tally.entry(outcome).or_default() += 1;
            }
        }
        eprintln!("{path}: {tally:?}");
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
