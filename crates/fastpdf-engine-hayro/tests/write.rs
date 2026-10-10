//! The writer (ADR 0013): page operations, stamps and copies, checked by
//! opening and rendering what was written, and its failure modes.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::path::{Path, PathBuf};

use common::{PageSpec, PdfBuilder, build, build_full, full_request, open, pixel, render};
use fastpdf_engine_api::write::{OutputContent, OutputSpec, PagePick, Stamp, WriteError, WriteJob};
use fastpdf_engine_api::{
    EngineDocument, GuardedDocument, PageIndex, PageRect, PixelFormat, Rotation,
};
use fastpdf_engine_hayro::write::execute;

/// A fresh directory for one test.
fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("fastpdf-write-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn save(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn text_page(text: &str) -> PageSpec {
    PageSpec::new(
        [0.0, 0.0, 612.0, 792.0],
        &format!("BT /F1 24 Tf 72 700 Td ({text}) Tj ET 0 0 1 rg 72 72 100 50 re f"),
    )
}

/// Three pages saying which one they are, and an outline whose only entry
/// points at page 2.
fn three_pages() -> Vec<u8> {
    let pages = [
        text_page("PAGE-ONE"),
        text_page("PAGE-TWO-SECRET"),
        text_page("PAGE-THREE"),
    ];
    // Objects: catalog 1, tree 2, font 3, (content, page) 4-9, outline 10-11.
    build_full(
        &pages,
        "/Outlines 10 0 R /PageLabels << /Nums [0 << /S /r >>] >>",
        |b| {
            b.add("<< /Type /Outlines /First 11 0 R /Last 11 0 R /Count 1 >>");
            b.add("<< /Title (Two) /Parent 10 0 R /Dest [7 0 R /Fit] >>");
        },
    )
}

fn picks(source: usize, pages: &[u32]) -> Vec<PagePick> {
    pages
        .iter()
        .map(|&p| PagePick::new(source, PageIndex::new(p)))
        .collect()
}

fn job(sources: Vec<PathBuf>, path: PathBuf, picks: Vec<PagePick>) -> WriteJob {
    WriteJob {
        sources,
        outputs: vec![OutputSpec {
            path,
            content: OutputContent::Pages(picks),
        }],
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// The full render of `page` at 50% (RGBA bytes).
fn rendered(doc: &GuardedDocument, page: u32) -> Vec<u8> {
    let req = full_request(doc, page, 0.5, Rotation::R0);
    render(doc, &req, PixelFormat::default())
        .unwrap()
        .into_data()
}

#[test]
fn extracted_pages_leave_the_rest_behind() {
    let dir = scratch("extract");
    let source = save(&dir, "in.pdf", &three_pages());
    let out = dir.join("out.pdf");
    let report = execute(&job(vec![source.clone()], out.clone(), picks(0, &[0, 2]))).unwrap();
    assert_eq!(report.outputs.len(), 1);
    assert_eq!(report.outputs[0].pages, 2);

    let bytes = std::fs::read(&out).unwrap();
    assert_eq!(report.outputs[0].bytes, bytes.len() as u64);
    assert!(contains(&bytes, b"PAGE-ONE") && contains(&bytes, b"PAGE-THREE"));
    assert!(
        !contains(&bytes, b"PAGE-TWO-SECRET"),
        "a deleted page's content must not survive"
    );
    // The outline is kept; its entry for the deleted page lost its target.
    assert!(contains(&bytes, b"/Dest [null /Fit]"));
    // Page labels no longer match the pages.
    assert!(!contains(&bytes, b"/PageLabels"));

    let original = open(std::fs::read(&source).unwrap());
    let written = open(bytes);
    assert_eq!(written.page_count(), 2);
    assert_eq!(rendered(&written, 0), rendered(&original, 0));
    assert_eq!(rendered(&written, 1), rendered(&original, 2));
}

#[test]
fn pages_are_reordered_rotated_and_duplicated() {
    let dir = scratch("reorder");
    let source = save(&dir, "in.pdf", &three_pages());
    let out = dir.join("out.pdf");
    let mut picked = picks(0, &[2, 0, 0]);
    picked[1].rotate = Rotation::R90;
    execute(&job(vec![source.clone()], out.clone(), picked)).unwrap();

    let original = open(std::fs::read(&source).unwrap());
    let written = open(std::fs::read(&out).unwrap());
    assert_eq!(written.page_count(), 3);
    let rotation = |p: u32| written.page_info(PageIndex::new(p)).unwrap().rotation;
    assert_eq!(
        [rotation(0), rotation(1), rotation(2)],
        [Rotation::R0, Rotation::R90, Rotation::R0]
    );
    assert_eq!(rendered(&written, 0), rendered(&original, 2));
    assert_eq!(rendered(&written, 2), rendered(&original, 0));
}

/// Two pages with form fields: one on page 1, and on page 2 a field that
/// is its own widget plus a parent field whose widget is a kid. The
/// structure tree also points at a page 2 widget.
fn form_pages() -> Vec<u8> {
    // Objects: catalog 1, tree 2, font 3, (content, page) 4-7, then 8-13.
    let mut first = text_page("PAGE-ONE");
    first.extra = "/Annots [8 0 R]".into();
    let mut second = text_page("PAGE-TWO");
    second.extra = "/Annots [9 0 R 11 0 R]".into();
    build_full(
        &[first, second],
        "/AcroForm << /Fields [8 0 R 9 0 R 10 0 R] /CO [9 0 R] >> /StructTreeRoot 12 0 R",
        |b| {
            b.add("<< /Type /Annot /Subtype /Widget /FT /Tx /T (kept) /V (KEPT-VALUE) /Rect [72 72 200 100] /P 5 0 R >>");
            b.add("<< /Type /Annot /Subtype /Widget /FT /Tx /T (gone) /V (SECRET-VALUE) /Rect [72 72 200 100] /P 7 0 R >>");
            b.add("<< /FT /Tx /T (parent) /V (SECRET-PARENT) /Kids [11 0 R] >>");
            b.add("<< /Type /Annot /Subtype /Widget /Parent 10 0 R /Rect [72 120 200 150] /P 7 0 R >>");
            b.add("<< /Type /StructTreeRoot /K [13 0 R] >>");
            b.add("<< /Type /StructElem /S /Form /K << /Type /OBJR /Obj 9 0 R >> >>");
        },
    )
}

#[test]
fn form_fields_of_deleted_pages_leave_with_them() {
    let dir = scratch("forms");
    let source = save(&dir, "in.pdf", &form_pages());
    let out = dir.join("out.pdf");
    execute(&job(vec![source.clone()], out.clone(), picks(0, &[0]))).unwrap();
    let bytes = std::fs::read(&out).unwrap();
    assert!(contains(&bytes, b"KEPT-VALUE"));
    assert!(!contains(&bytes, b"SECRET-VALUE"), "a widget of page 2");
    assert!(
        !contains(&bytes, b"SECRET-PARENT"),
        "a field without widgets left"
    );
    assert!(
        contains(&bytes, b"/Obj null"),
        "the structure tree lost the widget"
    );
    // The form lists exactly the kept field, in the output's numbering.
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let fields = text
        .split("/Fields [")
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .expect("a form with fields");
    let number = fields
        .strip_suffix(" 0 R")
        .expect("one reference")
        .trim()
        .to_owned();
    let object = text
        .split(&format!("\n{number} 0 obj\n"))
        .nth(1)
        .and_then(|rest| rest.split("endobj").next())
        .expect("the field object");
    assert!(object.contains("KEPT-VALUE"), "{object}");
    assert_eq!(open(bytes).page_count(), 1);

    // Every page kept (reordered): the form is untouched.
    execute(&job(vec![source], out.clone(), picks(0, &[1, 0]))).unwrap();
    let bytes = std::fs::read(&out).unwrap();
    for value in [&b"KEPT-VALUE"[..], b"SECRET-VALUE", b"SECRET-PARENT"] {
        assert!(contains(&bytes, value));
    }
}

#[test]
fn nested_fields_of_deleted_pages_leave_too() {
    // A parent field with a widget on page 1 and a child field whose only
    // widget is on page 2.
    let mut first = text_page("PAGE-ONE");
    first.extra = "/Annots [8 0 R 10 0 R]".into();
    let mut second = text_page("PAGE-TWO");
    second.extra = "/Annots [12 0 R]".into();
    let bytes = build_full(
        &[first, second],
        "/AcroForm << /Fields [8 0 R 9 0 R] >>",
        |b| {
            b.add("<< /Type /Annot /Subtype /Widget /FT /Tx /T (kept) /V (KEPT-VALUE) /Rect [72 72 200 100] /P 5 0 R >>");
            b.add("<< /FT /Tx /T (parent) /V (PARENT-VALUE) /Kids [10 0 R 11 0 R] >>");
            b.add(
                "<< /Type /Annot /Subtype /Widget /Parent 9 0 R /Rect [72 120 200 150] /P 5 0 R >>",
            );
            b.add("<< /T (child) /Parent 9 0 R /V (SECRET-NESTED) /Kids [12 0 R] >>");
            b.add("<< /Type /Annot /Subtype /Widget /Parent 11 0 R /Rect [72 120 200 150] /P 7 0 R >>");
        },
    );
    let dir = scratch("nested-forms");
    let source = save(&dir, "in.pdf", &bytes);
    let out = dir.join("out.pdf");
    execute(&job(vec![source], out.clone(), picks(0, &[0]))).unwrap();
    let bytes = std::fs::read(&out).unwrap();
    assert!(contains(&bytes, b"KEPT-VALUE") && contains(&bytes, b"PARENT-VALUE"));
    assert!(
        !contains(&bytes, b"SECRET-NESTED"),
        "a child field of page 2"
    );
    assert_eq!(open(bytes).page_count(), 1);
}

#[test]
fn merged_documents_keep_hidden_layers_hidden() {
    // A red square in a layer that the document hides by default.
    let mut page = PageSpec::new(
        [0.0, 0.0, 612.0, 792.0],
        "/OC /L1 BDC 1 0 0 rg 100 100 200 200 re f EMC",
    );
    page.resources = "/Properties << /L1 8 0 R >>".into();
    let layered = build_full(
        &[page],
        "/OCProperties << /OCGs [8 0 R] /D << /OFF [8 0 R] >> >>",
        |b| {
            b.add("<< /Type /OCG /Name (Hidden) >>");
        },
    );
    let dir = scratch("layers");
    let a = save(&dir, "a.pdf", &layered);
    let b = save(&dir, "b.pdf", &build(&[text_page("OTHER-DOCUMENT")]));
    let out = dir.join("merged.pdf");
    execute(&WriteJob {
        sources: vec![a, b],
        outputs: vec![OutputSpec {
            path: out.clone(),
            content: OutputContent::Merge(vec![0, 1]),
        }],
    })
    .unwrap();
    let bytes = std::fs::read(&out).unwrap();
    assert!(contains(&bytes, b"/OCProperties") && contains(&bytes, b"/OFF ["));
    let written = open(bytes);
    let req = full_request(&written, 0, 1.0, Rotation::R0);
    let pm = render(&written, &req, PixelFormat::Rgba8Premultiplied).unwrap();
    assert!(is_white(pixel(&pm, 200, 592)), "{:?}", pixel(&pm, 200, 592));
}

#[test]
fn layers_hidden_by_an_off_base_state_stay_hidden() {
    let mut page = PageSpec::new(
        [0.0, 0.0, 612.0, 792.0],
        "/OC /L1 BDC 1 0 0 rg 100 100 200 200 re f EMC",
    );
    page.resources = "/Properties << /L1 8 0 R >>".into();
    let layered = build_full(
        &[page],
        "/OCProperties << /OCGs [8 0 R 9 0 R] /D << /BaseState /OFF /ON [9 0 R] >> >>",
        |b| {
            b.add("<< /Type /OCG /Name (Hidden) >>");
            b.add("<< /Type /OCG /Name (Shown) >>");
        },
    );
    let dir = scratch("layers-off");
    let a = save(&dir, "a.pdf", &layered);
    let b = save(&dir, "b.pdf", &build(&[text_page("OTHER")]));
    let out = dir.join("merged.pdf");
    execute(&WriteJob {
        sources: vec![a, b],
        outputs: vec![OutputSpec {
            path: out.clone(),
            content: OutputContent::Merge(vec![0, 1]),
        }],
    })
    .unwrap();
    let written = open(std::fs::read(&out).unwrap());
    let req = full_request(&written, 0, 1.0, Rotation::R0);
    let pm = render(&written, &req, PixelFormat::Rgba8Premultiplied).unwrap();
    assert!(is_white(pixel(&pm, 200, 592)), "{:?}", pixel(&pm, 200, 592));
}

#[test]
fn structure_elements_of_deleted_pages_leave_the_tree() {
    // Tagged pages: each paragraph's actual text names its page.
    let first = text_page("PAGE-ONE");
    let second = text_page("PAGE-TWO");
    let bytes = build_full(
        &[first, second],
        "/StructTreeRoot 8 0 R /MarkInfo << /Marked true >>",
        |b| {
            b.add("<< /Type /StructTreeRoot /K [9 0 R 10 0 R 11 0 R] >>");
            b.add("<< /Type /StructElem /S /P /Pg 5 0 R /K 0 /ActualText (KEPT-TEXT) >>");
            b.add("<< /Type /StructElem /S /P /Pg 7 0 R /K 0 /ActualText (SECRET-TEXT) >>");
            b.add("<< /Type /StructElem /S /Div /K [12 0 R] >>");
            b.add("<< /Type /StructElem /S /P /K << /Type /MCR /Pg 7 0 R /MCID 1 >> /Alt (SECRET-ALT) >>");
        },
    );
    let dir = scratch("structure");
    let source = save(&dir, "in.pdf", &bytes);
    let out = dir.join("out.pdf");
    execute(&job(vec![source], out.clone(), picks(0, &[0]))).unwrap();
    let bytes = std::fs::read(&out).unwrap();
    assert!(contains(&bytes, b"KEPT-TEXT"));
    assert!(!contains(&bytes, b"SECRET-TEXT"), "an element of page 2");
    assert!(
        !contains(&bytes, b"SECRET-ALT"),
        "a nested element of page 2"
    );
    assert_eq!(open(bytes).page_count(), 1);
}

#[test]
fn merged_documents_keep_their_forms() {
    let form = |value: &str| {
        let mut page = text_page("FORM");
        page.extra = "/Annots [6 0 R]".into();
        build_full(
            &[page],
            "/AcroForm << /Fields [6 0 R] /DA (/Helv 0 Tf 0 g) >>",
            |b| {
                b.add(format!(
                "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /V ({value}) /Rect [72 72 200 100] /P 5 0 R >>"
            ));
            },
        )
    };
    let dir = scratch("merge-forms");
    let a = save(&dir, "a.pdf", &form("FIRST-VALUE"));
    let b = save(&dir, "b.pdf", &form("SECOND-VALUE"));
    let out = dir.join("merged.pdf");
    execute(&WriteJob {
        sources: vec![a, b],
        outputs: vec![OutputSpec {
            path: out.clone(),
            content: OutputContent::Merge(vec![0, 1]),
        }],
    })
    .unwrap();
    let bytes = std::fs::read(&out).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("/AcroForm <</Fields ["), "the merged form");
    assert!(text.contains("/DA (/Helv 0 Tf 0 g)"));
    // The second "name" became "name-2" (written as hex).
    assert!(text.contains("/T <6E616D652D32>"), "renamed");
    assert!(contains(&bytes, b"FIRST-VALUE") && contains(&bytes, b"SECOND-VALUE"));
}

#[test]
fn every_page_in_order_keeps_the_page_labels() {
    let dir = scratch("labels");
    let source = save(&dir, "in.pdf", &three_pages());
    let out = dir.join("out.pdf");
    execute(&job(vec![source], out.clone(), picks(0, &[0, 1, 2]))).unwrap();
    let bytes = std::fs::read(&out).unwrap();
    assert!(contains(&bytes, b"/PageLabels"));
    assert!(
        contains(&bytes, b"/Dest [4 0 R /Fit]"),
        "the outline target"
    );
}

#[test]
fn inherited_attributes_survive_a_flat_tree() {
    // Root and middle node carry resources, the media box and the
    // rotation; the page itself has only its contents.
    let mut b = PdfBuilder::new();
    let catalog = b.reserve();
    let root = b.reserve();
    let middle = b.reserve();
    let font = b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    let content = b.add_stream(
        "",
        b"BT /F1 30 Tf 20 300 Td (INHERITED) Tj ET 1 0 0 rg 20 20 60 60 re f".to_vec(),
    );
    let page = b.add(format!(
        "<< /Type /Page /Parent {middle} 0 R /Contents {content} 0 R >>"
    ));
    b.set(
        middle,
        format!("<< /Type /Pages /Parent {root} 0 R /Kids [{page} 0 R] /Count 1 /Rotate 90 >>"),
    );
    b.set(
        root,
        format!(
            "<< /Type /Pages /Kids [{middle} 0 R] /Count 1 /MediaBox [0 0 300 400] \
             /Resources << /Font << /F1 {font} 0 R >> >> >>"
        ),
    );
    b.set(catalog, format!("<< /Type /Catalog /Pages {root} 0 R >>"));
    let dir = scratch("inherit");
    let source = save(&dir, "in.pdf", &b.finish(catalog));
    let out = dir.join("out.pdf");
    execute(&job(vec![source.clone()], out.clone(), picks(0, &[0]))).unwrap();

    let original = open(std::fs::read(&source).unwrap());
    let written = open(std::fs::read(&out).unwrap());
    assert_eq!(
        written.page_info(PageIndex::FIRST).unwrap(),
        original.page_info(PageIndex::FIRST).unwrap()
    );
    assert_eq!(
        written.page_info(PageIndex::FIRST).unwrap().rotation,
        Rotation::R90
    );
    assert_eq!(rendered(&written, 0), rendered(&original, 0));
}

#[test]
fn documents_are_merged_without_their_catalogs() {
    let dir = scratch("merge");
    let a = save(&dir, "a.pdf", &three_pages());
    let b = save(&dir, "b.pdf", &build(&[text_page("OTHER-DOCUMENT")]));
    let out = dir.join("merged.pdf");
    let mut picked = picks(0, &[0]);
    picked.extend(picks(1, &[0]));
    picked.extend(picks(0, &[2]));
    execute(&job(vec![a.clone(), b.clone()], out.clone(), picked)).unwrap();

    let bytes = std::fs::read(&out).unwrap();
    assert!(!contains(&bytes, b"PAGE-TWO-SECRET"));
    assert!(
        !contains(&bytes, b"/Outlines"),
        "only a single source keeps its catalog"
    );
    let written = open(bytes);
    assert_eq!(written.page_count(), 3);
    let other = open(std::fs::read(&b).unwrap());
    assert_eq!(rendered(&written, 1), rendered(&other, 0));
}

#[test]
fn whole_files_are_merged_without_page_counts() {
    let dir = scratch("merge-all");
    let a = save(&dir, "a.pdf", &three_pages());
    let b = save(&dir, "b.pdf", &build(&[text_page("OTHER-DOCUMENT")]));
    let out = dir.join("merged.pdf");
    let report = execute(&WriteJob {
        sources: vec![a, b],
        outputs: vec![OutputSpec {
            path: out.clone(),
            content: OutputContent::Merge(vec![1, 0]),
        }],
    })
    .unwrap();
    assert_eq!(report.outputs[0].pages, 4);
    let bytes = std::fs::read(&out).unwrap();
    assert!(contains(&bytes, b"PAGE-TWO-SECRET"), "every page of both");
    assert_eq!(open(bytes).page_count(), 4);
}

/// A 20 x 20 PNG: top half red, bottom half blue, fully opaque, or with a
/// transparent left column when `hole`.
fn two_color_png(hole: bool) -> Vec<u8> {
    let mut img = image::RgbaImage::new(20, 20);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = if hole && x < 5 {
            image::Rgba([0, 0, 0, 0])
        } else if y < 10 {
            image::Rgba([255, 0, 0, 255])
        } else {
            image::Rgba([0, 0, 255, 255])
        };
    }
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn is_red(p: [u8; 4]) -> bool {
    p[0] > 200 && p[1] < 60 && p[2] < 60
}

fn is_blue(p: [u8; 4]) -> bool {
    p[0] < 60 && p[1] < 60 && p[2] > 200
}

fn is_white(p: [u8; 4]) -> bool {
    p[0] > 240 && p[1] > 240 && p[2] > 240
}

#[test]
fn stamps_are_drawn_over_the_page() {
    let dir = scratch("stamp");
    let source = save(&dir, "in.pdf", &three_pages());
    let seal = save(&dir, "seal.png", &two_color_png(true));
    let out = dir.join("stamped.pdf");
    let mut picked = picks(0, &[0, 1]);
    // Page 1: upright, 100 pt square at (300, 300) from the top left.
    picked[0].stamps.push(Stamp {
        image: seal.clone(),
        rect: PageRect::new(300.0, 300.0, 400.0, 400.0),
        rotate: Rotation::R0,
    });
    // Page 2: the same image turned clockwise (its top now faces right).
    picked[1].stamps.push(Stamp {
        image: seal.clone(),
        rect: PageRect::new(300.0, 300.0, 400.0, 400.0),
        rotate: Rotation::R90,
    });
    execute(&job(vec![source], out.clone(), picked)).unwrap();

    let bytes = std::fs::read(&out).unwrap();
    assert!(contains(&bytes, b"/FastPdfStamp0 Do"));
    assert!(contains(&bytes, b"/SMask"), "the transparent column");
    let written = open(bytes);
    let at = |page: u32, x: u32, y: u32| {
        let req = full_request(&written, page, 1.0, Rotation::R0);
        let pm = render(&written, &req, PixelFormat::Rgba8Premultiplied).unwrap();
        pixel(&pm, x, y)
    };
    // Upright: red above, blue below, the transparent column shows paper.
    assert!(is_red(at(0, 350, 320)), "{:?}", at(0, 350, 320));
    assert!(is_blue(at(0, 350, 380)), "{:?}", at(0, 350, 380));
    assert!(is_white(at(0, 305, 350)), "{:?}", at(0, 305, 350));
    assert!(is_white(at(0, 250, 350)), "outside the stamp");
    // Turned clockwise: red on the right, blue on the left, the
    // transparent column (the image's left edge) along the top.
    assert!(is_red(at(1, 380, 350)), "{:?}", at(1, 380, 350));
    assert!(is_blue(at(1, 320, 350)), "{:?}", at(1, 320, 350));
    assert!(is_white(at(1, 350, 305)), "{:?}", at(1, 350, 305));
    // The page's own content is still there (the blue box at the bottom).
    assert!(is_blue(at(0, 120, 792 - 97)), "{:?}", at(0, 120, 792 - 97));
}

#[test]
fn copies_are_byte_identical() {
    let dir = scratch("copy");
    let original = three_pages();
    let source = save(&dir, "in.pdf", &original);
    let out = dir.join("copy.pdf");
    let report = execute(&WriteJob {
        sources: vec![source],
        outputs: vec![OutputSpec {
            path: out.clone(),
            content: OutputContent::Copy { source: 0 },
        }],
    })
    .unwrap();
    assert_eq!(report.outputs[0].pages, 3);
    assert_eq!(std::fs::read(&out).unwrap(), original);
}

#[test]
fn encrypted_sources_are_refused() {
    let dir = scratch("encrypted");
    let bytes = common::build_with(&[text_page("x")], |b| b.encrypt_rc4("", "owner"));
    let source = save(&dir, "in.pdf", &bytes);
    let err = execute(&job(vec![source], dir.join("out.pdf"), picks(0, &[0]))).unwrap_err();
    assert_eq!(err, WriteError::Encrypted { index: 0 });
}

#[test]
fn a_failed_job_leaves_nothing_behind() {
    let dir = scratch("all-or-nothing");
    let source = save(&dir, "in.pdf", &three_pages());
    let job = WriteJob {
        sources: vec![source],
        outputs: vec![
            OutputSpec {
                path: dir.join("first.pdf"),
                content: OutputContent::Pages(picks(0, &[0])),
            },
            OutputSpec {
                path: dir.join("second.pdf"),
                content: OutputContent::Pages(picks(0, &[7])),
            },
        ],
    };
    let err = execute(&job).unwrap_err();
    assert_eq!(
        err,
        WriteError::NoSuchPage {
            source: 0,
            page: PageIndex::new(7)
        }
    );
    let left: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, ["in.pdf"]);
    // A missing stamp image fails the same way.
    let source = dir.join("in.pdf");
    let mut picked = picks(0, &[0]);
    picked[0].stamps.push(Stamp {
        image: dir.join("missing.png"),
        rect: PageRect::new(0.0, 0.0, 10.0, 10.0),
        rotate: Rotation::R0,
    });
    let err = execute(&job_one(source, dir.join("x.pdf"), picked)).unwrap_err();
    assert!(matches!(err, WriteError::Image { .. }), "{err:?}");
    assert!(!dir.join("x.pdf").exists());
}

fn job_one(source: PathBuf, path: PathBuf, picks: Vec<PagePick>) -> WriteJob {
    job(vec![source], path, picks)
}

#[test]
fn sources_are_never_overwritten() {
    let dir = scratch("overwrite");
    let source = save(&dir, "in.pdf", &three_pages());
    let err = execute(&job(vec![source.clone()], source.clone(), picks(0, &[0]))).unwrap_err();
    assert!(matches!(err, WriteError::Job(_)), "{err:?}");
    assert_eq!(std::fs::read(&source).unwrap(), three_pages());
}

/// Object streams, cross-reference streams and a broken cross-reference
/// table, from the generated corpus (skipped when it is not generated).
#[test]
fn generated_fixtures_are_rewritten() {
    let dir = scratch("fixtures");
    for (name, pages) in [
        (
            "large-page-count/objstm-xrefstream-2000p.pdf",
            &[0u32, 1999][..],
        ),
        ("malformed/bad-xref-offsets.pdf", &[0, 1, 2][..]),
        (
            "traditional-chinese/gov-letter-embedded-type0.pdf",
            &[1, 0][..],
        ),
    ] {
        let Some(bytes) = common::fixture(name) else {
            continue;
        };
        let source = save(&dir, "in.pdf", &bytes);
        let out = dir.join("out.pdf");
        execute(&job(vec![source], out.clone(), picks(0, pages))).unwrap();
        let original = open(bytes);
        let written = open(std::fs::read(&out).unwrap());
        assert_eq!(written.page_count() as usize, pages.len(), "{name}");
        for (i, &p) in pages.iter().enumerate() {
            assert_eq!(
                rendered(&written, i as u32),
                rendered(&original, p),
                "{name} page {}",
                p + 1
            );
        }
    }
}

/// A page rendered whole at 50%, or why not (geometry or render error).
fn page_render(
    doc: &GuardedDocument,
    page: u32,
) -> Result<fastpdf_engine_api::Pixmap, fastpdf_engine_api::EngineError> {
    let info = doc.page_info(PageIndex::new(page))?;
    let req = fastpdf_engine_api::RenderRequest::full_page(
        PageIndex::new(page),
        info.size,
        info.rotation,
        Rotation::R0,
        common::scale(0.5),
    );
    render(doc, &req, PixelFormat::default())
}

/// Every unencrypted corpus file rewritten page for page must render the
/// same. Slow: `cargo test -p fastpdf-engine-hayro --test write -- --ignored`.
#[test]
#[ignore = "renders the whole generated corpus twice"]
fn corpus_round_trip() {
    let Some(manifest) = common::fixture("manifest.json") else {
        return;
    };
    let manifest = String::from_utf8(manifest).unwrap();
    let dir = scratch("corpus");
    let mut checked = 0;
    let mut failures = Vec::new();
    for path in manifest
        .split("\"path\": \"")
        .skip(1)
        .filter_map(|s| s.split('"').next())
    {
        let Some(bytes) = common::fixture(path) else {
            continue;
        };
        let Ok(original) = common::open_with(bytes.clone(), None, Default::default()) else {
            continue;
        };
        let source = save(&dir, "in.pdf", &bytes);
        let out = dir.join("out.pdf");
        let all: Vec<u32> = (0..original.page_count()).collect();
        match execute(&job(vec![source], out.clone(), picks(0, &all))) {
            Err(WriteError::Encrypted { .. }) => continue,
            Err(e) => {
                failures.push(format!("{path}: {e}"));
                continue;
            }
            Ok(_) => {}
        }
        let written = open(std::fs::read(&out).unwrap());
        for p in all {
            let (a, b) = (page_render(&original, p), page_render(&written, p));
            match (a, b) {
                (Ok(a), Ok(b)) if a.data() == b.data() => {}
                (Err(_), Err(_)) => {}
                (a, b) => failures.push(format!(
                    "{path} page {}: {:?} vs {:?}",
                    p + 1,
                    a.map(|_| "rendered"),
                    b.map(|_| "rendered")
                )),
            }
            checked += 1;
        }
    }
    eprintln!("{checked} pages checked");
    assert!(failures.is_empty(), "{failures:#?}");
}
