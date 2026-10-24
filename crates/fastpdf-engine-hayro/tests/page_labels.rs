//! Page labels read from the catalog's `/PageLabels` number tree, including
//! hostile trees that must stay bounded.

mod common;

use std::time::{Duration, Instant};

use common::{PageSpec, build_full, fixture, open};
use fastpdf_engine_api::{EngineDocument, PageIndex, PageLabelStyle, PageLabels};

/// A document of `pages` empty pages whose catalog gets `catalog_extra`;
/// `objects` are placed at fixed object numbers (100 and up).
fn document(pages: u32, catalog_extra: &str, objects: &[(u32, String)]) -> Vec<u8> {
    let specs: Vec<PageSpec> = (0..pages)
        .map(|_| PageSpec::new([0.0, 0.0, 200.0, 200.0], ""))
        .collect();
    build_full(&specs, catalog_extra, |b| {
        let last = objects.iter().map(|(n, _)| *n).max().unwrap_or(0);
        while b.reserve() < last {}
        for (n, body) in objects {
            b.set(*n, body.clone());
        }
    })
}

fn labels_of(bytes: Vec<u8>) -> (PageLabels, Vec<Option<String>>) {
    let doc = open(bytes);
    let labels = match doc.page_labels() {
        Ok(labels) => labels,
        Err(e) => panic!("page labels failed: {e:?}"),
    };
    let all = (0..doc.page_count())
        .map(|p| labels.label(PageIndex::new(p)))
        .collect();
    (labels, all)
}

fn some(labels: &[&str]) -> Vec<Option<String>> {
    labels.iter().map(|s| Some((*s).to_owned())).collect()
}

#[test]
fn page_labels_from_a_nums_array() {
    let bytes = document(
        8,
        "/PageLabels << /Nums [0 << /S /r >> 3 << /S /D >> 6 << /S /A /P (App-) >>] >>",
        &[],
    );
    let (labels, all) = labels_of(bytes);
    assert_eq!(
        all,
        some(&["i", "ii", "iii", "1", "2", "3", "App-A", "App-B"])
    );
    assert!(!labels.is_plain_numbers());
    assert_eq!(labels.find("App-B"), Some(PageIndex::new(7)));
}

#[test]
fn page_labels_from_a_kids_tree() {
    // 附錄- in UTF-16BE.
    let prefix = "FEFF96449304002D";
    let bytes = document(
        8,
        "/PageLabels 100 0 R",
        &[
            (100, "<< /Kids [101 0 R 102 0 R] >>".into()),
            (
                101,
                "<< /Limits [0 2] /Nums [0 << /P (Cover) >> 1 103 0 R] >>".into(),
            ),
            (102, "<< /Limits [3 7] /Kids [104 0 R] >>".into()),
            (103, "<< /Type /PageLabel /S /R >>".into()),
            (
                104,
                format!(
                    "<< /Limits [3 7] /Nums [3 << /S /a >> 5 << /S /D /P <{prefix}> /St 5 >>] >>"
                ),
            ),
        ],
    );
    let (_, all) = labels_of(bytes);
    assert_eq!(
        all,
        some(&["Cover", "I", "II", "a", "b", "附錄-5", "附錄-6", "附錄-7"])
    );
}

#[test]
fn missing_or_plain_page_labels() {
    let (labels, all) = labels_of(document(3, "", &[]));
    assert!(labels.is_empty());
    assert_eq!(all, vec![None, None, None]);

    let (labels, all) = labels_of(document(3, "/PageLabels << /Nums [0 << /S /D >>] >>", &[]));
    assert!(labels.is_plain_numbers());
    assert_eq!(all, some(&["1", "2", "3"]));

    // Not a dictionary, or a tree without entries.
    for extra in ["/PageLabels 7", "/PageLabels << /Kids [5 0 R] >>"] {
        let (labels, _) = labels_of(document(2, extra, &[]));
        assert!(labels.is_empty(), "{extra}");
    }
}

#[test]
fn malformed_keys_and_values_are_skipped() {
    // 10,000 characters of UTF-16BE with line feeds and right-to-left
    // overrides.
    let long: String = (0..10_000)
        .map(|i| match i % 50 {
            0 => "000A",
            1 => "202E",
            _ => "0078",
        })
        .collect();
    let nums = format!(
        "-1 << /S /D >> 2.5 << /S /D >> 1000000000000 << /S /D >> (3) << /S /D >> \
         1 null 2 999 0 R 3 << /S /D /St -7 >> 4 << /S /R /St 99999999999 >> \
         5 << /S /Q /P (Q-) >> 6 << /S /a /P <FEFF{long}> >> 7 9 8 0 R"
    );
    let bytes = document(8, &format!("/PageLabels << /Nums [{nums}] >>"), &[]);
    let started = Instant::now();
    let (labels, all) = labels_of(bytes);
    assert!(started.elapsed() < Duration::from_secs(2));
    let ranges = labels.ranges();
    let pages: Vec<u32> = ranges.iter().map(|r| r.first_page.get()).collect();
    assert_eq!(pages, vec![3, 4, 5, 6]);
    assert_eq!(ranges[0].start, 1);
    assert_eq!(ranges[1].start, u32::MAX);
    assert_eq!(ranges[2].style, PageLabelStyle::None);
    let prefix = &ranges[3].prefix;
    assert!(
        prefix.len() <= PageLabels::MAX_PREFIX_BYTES,
        "{}",
        prefix.len()
    );
    assert!(
        prefix.starts_with(" xx") && prefix.ends_with('\u{2026}'),
        "{prefix:?}"
    );
    assert!(
        !prefix.contains('\n') && !prefix.contains('\u{202E}'),
        "{prefix:?}"
    );
    assert_eq!(all[..3], [None, None, None]);
    assert_eq!(all[3].as_deref(), Some("1"));
    assert_eq!(all[4].as_deref(), Some("4294967295"));
    assert_eq!(all[5].as_deref(), Some("Q-"));
    assert_eq!(all[6], Some(format!("{prefix}a")));
    assert_eq!(all[7], Some(format!("{prefix}b")));
}

#[test]
fn hostile_number_trees_are_bounded() {
    let started = Instant::now();

    // A node that is its own kid, and a cycle through the root.
    let (_, all) = labels_of(document(
        3,
        "/PageLabels 100 0 R",
        &[
            (100, "<< /Kids [100 0 R 101 0 R] >>".into()),
            (
                101,
                "<< /Kids [100 0 R 101 0 R] /Nums [0 << /S /r >>] >>".into(),
            ),
        ],
    ));
    assert_eq!(all, some(&["i", "ii", "iii"]));

    // A chain 200 levels deep: labels below the depth limit are ignored.
    let mut chain: Vec<(u32, String)> = (0..200)
        .map(|i| (100 + i, format!("<< /Kids [{} 0 R] >>", 101 + i)))
        .collect();
    chain[3].1 = "<< /Kids [104 0 R] /Nums [0 << /S /A >>] >>".into();
    chain.push((300, "<< /Nums [1 << /S /r >>] >>".into()));
    let (_, all) = labels_of(document(2, "/PageLabels 100 0 R", &chain));
    assert_eq!(all, some(&["A", "B"]));

    // 200,000 pairs in one array: only a bounded number are looked at.
    let mut nums = String::from("0 << /S /D /P (n) >> ");
    for i in 0..200_000u32 {
        nums.push_str(&format!("{} << /S /D >> ", i % 7 + 1));
    }
    let (labels, _) = labels_of(document(
        4,
        &format!("/PageLabels << /Nums [{nums}] >>"),
        &[],
    ));
    assert_eq!(labels.ranges().len(), 4);
    assert_eq!(labels.label(PageIndex::FIRST).as_deref(), Some("n1"));

    // Shared huge objects are read once: 50,000 keys that all point at one
    // value with a 1 MB prefix, and 9,000 kids that are all the same node.
    let huge = "y".repeat(1 << 20);
    let mut nums = String::new();
    for i in 0..50_000u32 {
        nums.push_str(&format!("{} 101 0 R ", i % 999 + 1));
    }
    let kids = "102 0 R ".repeat(9_000);
    let (labels, all) = labels_of(document(
        1000,
        "/PageLabels 100 0 R",
        &[
            (100, format!("<< /Kids [{kids}] /Nums [{nums}] >>")),
            (101, format!("<< /S /D /P ({huge}) >>")),
            (102, format!("<< /Kids [{kids}] /Nums [0 << /S /r >>] >>")),
        ],
    ));
    assert_eq!(labels.ranges().len(), 1000);
    assert_eq!(all[0].as_deref(), Some("i"));
    let prefix = &labels.ranges()[999].prefix;
    assert!(prefix.len() <= PageLabels::MAX_PREFIX_BYTES);
    assert!(prefix.starts_with("yyy") && prefix.ends_with('\u{2026}'));

    // Objects of the wrong type are expensive to reject (they are scanned
    // to their end), so none is resolved twice: 3,000 nodes whose /Nums is
    // the same 1 MB dictionary, 50,000 values that are the same 1 MB array,
    // and styles, starts and prefixes behind references.
    let huge_dict = format!("<< /Big ({huge}) >>");
    let huge_array = format!("[({huge})]");
    let mut objects: Vec<(u32, String)> = (0..3_000)
        .map(|i| (200 + i, "<< /Nums 101 0 R >>".to_owned()))
        .collect();
    let kids: String = (0..3_000).map(|i| format!("{} 0 R ", 200 + i)).collect();
    let mut nums = String::from("0 << /S 102 0 R /St 103 0 R /P 104 0 R >> ");
    for i in 0..50_000u32 {
        nums.push_str(&format!("{} 102 0 R ", i % 3 + 1));
    }
    nums.push_str("4 << /S /D /P 104 0 R >>");
    objects.extend([
        (100, format!("<< /Kids [{kids}] /Nums [{nums}] >>")),
        (101, huge_dict),
        (102, huge_array),
        (103, "7".to_owned()),
        (104, "(Part-)".to_owned()),
    ]);
    let (_, all) = labels_of(document(6, "/PageLabels 100 0 R", &objects));
    // Indirect /S and /St are not followed; an indirect /P is.
    assert_eq!(
        all,
        vec![
            Some("Part-".into()),
            Some("Part-".into()),
            Some("Part-".into()),
            Some("Part-".into()),
            Some("Part-1".into()),
            Some("Part-2".into()),
        ]
    );

    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn page_labels_fixtures() {
    if let Some(bytes) = fixture("small-text/page-labels-mixed.pdf") {
        let (_, all) = labels_of(bytes);
        assert_eq!(
            all,
            some(&["i", "ii", "iii", "1", "2", "3", "App-A", "App-B"])
        );
    }
    if let Some(bytes) = fixture("small-text/page-labels-tree-cjk.pdf") {
        let (labels, all) = labels_of(bytes);
        assert_eq!(
            all,
            some(&[
                "Cover", "I", "II", "III", "a", "b", "1", "2", "附錄-5", "附錄-6", "1", "2"
            ])
        );
        // The first page labeled 1 in document order.
        assert_eq!(labels.find("1"), Some(PageIndex::new(6)));
    }
    if let Some(bytes) = fixture("malformed/page-labels-hostile.pdf") {
        let started = Instant::now();
        let (labels, all) = labels_of(bytes);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(all[1..], some(&["4294967295", "Q-"]));
        let first = &labels.ranges()[0].prefix;
        assert!(first.len() <= PageLabels::MAX_PREFIX_BYTES);
        assert_eq!(all[0], Some(format!("{first}i")));
    }
}
