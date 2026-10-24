use super::*;
use crate::{
    CancelToken, DocumentSource, EngineCapabilities, EngineDocument, EngineError, EngineInfo,
    OpenOptions, PageInfo, PageSize, PdfEngine, PixmapMut, RenderOutcome, RenderRequest, Rotation,
    SharedBytes, open_guarded,
};

fn range(first: u32, style: PageLabelStyle, prefix: &str, start: u32) -> PageLabelRange {
    PageLabelRange {
        first_page: PageIndex::new(first),
        style,
        prefix: prefix.to_owned(),
        start,
    }
}

/// i, ii, iii, 1, 2, 3, App-A, App-B
fn mixed() -> PageLabels {
    PageLabels::new(
        vec![
            range(0, PageLabelStyle::LowerRoman, "", 1),
            range(3, PageLabelStyle::Decimal, "", 1),
            range(6, PageLabelStyle::UpperLetters, "App-", 1),
        ],
        8,
    )
}

fn all_labels(labels: &PageLabels) -> Vec<Option<String>> {
    (0..labels.page_count())
        .map(|p| labels.label(PageIndex::new(p)))
        .collect()
}

#[test]
fn labels_are_formatted_per_range() {
    let labels = mixed();
    let expected = ["i", "ii", "iii", "1", "2", "3", "App-A", "App-B"];
    assert_eq!(
        all_labels(&labels),
        expected.map(|s| Some(s.to_owned())).to_vec()
    );
    assert_eq!(labels.label(PageIndex::new(8)), None);

    // Pages before the first range have no label.
    let late = PageLabels::new(vec![range(2, PageLabelStyle::Decimal, "p", 5)], 4);
    assert_eq!(
        all_labels(&late),
        vec![None, None, Some("p5".into()), Some("p6".into())]
    );

    // Prefix only, and an upper-roman range with a start value.
    let other = PageLabels::new(
        vec![
            range(0, PageLabelStyle::None, "Cover", 9),
            range(1, PageLabelStyle::UpperRoman, "", 4),
            range(3, PageLabelStyle::LowerLetters, "", 26),
        ],
        5,
    );
    let expected = ["Cover", "IV", "V", "z", "aa"];
    assert_eq!(
        all_labels(&other),
        expected.map(|s| Some(s.to_owned())).to_vec()
    );
}

#[test]
fn find_round_trips_every_page() {
    let labels = mixed();
    for page in 0..8 {
        let page = PageIndex::new(page);
        let label = labels.label(page).unwrap();
        assert_eq!(labels.find(&label), Some(page), "{label}");
    }
    // Case fallback.
    assert_eq!(labels.find("III"), Some(PageIndex::new(2)));
    assert_eq!(labels.find("app-b"), Some(PageIndex::new(7)));
    // Out of the range, non-canonical, unknown, empty.
    assert_eq!(labels.find("iv"), None);
    assert_eq!(labels.find("4"), None);
    assert_eq!(labels.find("iiii"), None);
    assert_eq!(labels.find("App-C"), None);
    assert_eq!(labels.find("01"), None);
    assert_eq!(labels.find(""), None);
    assert_eq!(labels.find(&"i".repeat(1000)), None);

    let cover = PageLabels::new(
        vec![
            range(0, PageLabelStyle::None, "Cover", 1),
            range(2, PageLabelStyle::Decimal, "附錄-", 5),
        ],
        4,
    );
    assert_eq!(cover.find("Cover"), Some(PageIndex::FIRST));
    assert_eq!(cover.find("cover"), Some(PageIndex::FIRST));
    assert_eq!(cover.find("附錄-6"), Some(PageIndex::new(3)));
    assert_eq!(cover.find("附錄-4"), None);
}

#[test]
fn duplicate_labels_find_the_first_page() {
    // Part I and Part II both restart at 1.
    let labels = PageLabels::new(
        vec![
            range(0, PageLabelStyle::Decimal, "", 1),
            range(3, PageLabelStyle::Decimal, "", 1),
        ],
        6,
    );
    assert_eq!(labels.label(PageIndex::new(3)).as_deref(), Some("1"));
    assert_eq!(labels.find("1"), Some(PageIndex::FIRST));
    assert_eq!(labels.find("3"), Some(PageIndex::new(2)));
    // An exact match in a later range wins over a case-insensitive one.
    let cased = PageLabels::new(
        vec![
            range(0, PageLabelStyle::UpperRoman, "", 1),
            range(2, PageLabelStyle::LowerRoman, "", 1),
        ],
        4,
    );
    assert_eq!(cased.find("i"), Some(PageIndex::new(2)));
    assert_eq!(cased.find("I"), Some(PageIndex::FIRST));
}

#[test]
fn normalization_drops_hostile_ranges() {
    let labels = PageLabels::new(
        vec![
            range(5, PageLabelStyle::Decimal, "late", 1),
            range(0, PageLabelStyle::LowerRoman, "first", 0),
            range(0, PageLabelStyle::Decimal, "duplicate", 1),
            range(9, PageLabelStyle::Decimal, "past the end", 1),
            range(2, PageLabelStyle::Decimal, "a\nb\u{202E}c", 3),
        ],
        6,
    );
    let ranges = labels.ranges();
    assert_eq!(ranges.len(), 3);
    assert_eq!(ranges[0].prefix, "first");
    assert_eq!(ranges[0].start, 1);
    assert_eq!(ranges[1].prefix, "a bc");
    assert_eq!(ranges[2].first_page, PageIndex::new(5));
    assert_eq!(PageLabels::new(labels.clone().into_ranges(), 6), labels);

    // CJK prefixes are cut on a character boundary.
    let long = PageLabels::new(
        vec![range(0, PageLabelStyle::None, &"附".repeat(100), 1)],
        1,
    );
    let prefix = &long.ranges()[0].prefix;
    assert!(prefix.len() <= PageLabels::MAX_PREFIX_BYTES);
    assert!(prefix.ends_with('\u{2026}'));
    assert_eq!(PageLabels::new(long.clone().into_ranges(), 1), long);

    let many: Vec<_> = (0..20_000)
        .rev()
        .map(|p| range(p, PageLabelStyle::Decimal, "", 1))
        .collect();
    let capped = PageLabels::new(many, 30_000);
    assert_eq!(capped.ranges().len(), PageLabels::MAX_RANGES);
    assert_eq!(capped.ranges()[0].first_page, PageIndex::FIRST);
    assert_eq!(
        capped.ranges().last().unwrap().first_page,
        PageIndex::new(PageLabels::MAX_RANGES as u32 - 1)
    );

    // Huge start values never overflow and fall back to decimal.
    let huge = PageLabels::new(
        vec![range(0, PageLabelStyle::UpperLetters, "", u32::MAX)],
        3,
    );
    assert_eq!(huge.label(PageIndex::new(2)).as_deref(), Some("4294967297"));
    assert_eq!(huge.find("4294967297"), Some(PageIndex::new(2)));
}

#[test]
fn plain_numbers_are_detected() {
    assert!(PageLabels::default().is_plain_numbers());
    assert!(PageLabels::default().is_empty());
    let plain = PageLabels::new(
        vec![
            range(0, PageLabelStyle::Decimal, "", 1),
            range(4, PageLabelStyle::Decimal, "", 5),
        ],
        9,
    );
    assert!(plain.is_plain_numbers());
    assert!(!mixed().is_plain_numbers());
    let restart = PageLabels::new(
        vec![
            range(0, PageLabelStyle::Decimal, "", 1),
            range(4, PageLabelStyle::Decimal, "", 1),
        ],
        9,
    );
    assert!(!restart.is_plain_numbers());
    let late = PageLabels::new(vec![range(1, PageLabelStyle::Decimal, "", 2)], 3);
    assert!(!late.is_plain_numbers());
}

/// An engine whose labels are unsorted and out of range, or that panics.
struct LabelEngine {
    panics: bool,
}

struct LabelDoc {
    panics: bool,
}

impl PdfEngine for LabelEngine {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            name: "labels",
            version: "0",
            capabilities: EngineCapabilities::default(),
        }
    }

    fn open(
        &self,
        _source: DocumentSource,
        _options: &OpenOptions,
    ) -> Result<Box<dyn EngineDocument>, EngineError> {
        Ok(Box::new(LabelDoc {
            panics: self.panics,
        }))
    }
}

impl EngineDocument for LabelDoc {
    fn page_count(&self) -> u32 {
        4
    }

    fn page_info(&self, _page: PageIndex) -> Result<PageInfo, EngineError> {
        Ok(PageInfo {
            size: PageSize::LETTER,
            rotation: Rotation::R0,
        })
    }

    fn render(
        &self,
        _request: &RenderRequest,
        _target: &mut PixmapMut<'_>,
        _cancel: &CancelToken,
    ) -> Result<RenderOutcome, EngineError> {
        Ok(RenderOutcome::default())
    }

    fn page_labels(&self) -> Result<PageLabels, EngineError> {
        if self.panics {
            panic!("number tree exploded");
        }
        // Not normalized: an engine may hand back anything.
        Ok(PageLabels {
            ranges: vec![
                range(7, PageLabelStyle::Decimal, "", 1),
                range(2, PageLabelStyle::Decimal, "x\u{0}", 0),
                range(0, PageLabelStyle::LowerRoman, "", 1),
            ],
            page_count: 99,
        })
    }
}

fn open_labels(panics: bool) -> crate::GuardedDocument {
    open_guarded(
        &LabelEngine { panics },
        DocumentSource::from_bytes(SharedBytes::from_vec(b"x".to_vec())),
        &OpenOptions::default(),
    )
    .unwrap()
}

#[test]
fn guarded_labels_are_normalized_and_contained() {
    let doc = open_labels(false);
    let labels = doc.page_labels().unwrap();
    assert_eq!(labels.page_count(), 4);
    assert_eq!(labels.ranges().len(), 2);
    assert_eq!(labels.label(PageIndex::new(3)).as_deref(), Some("x 2"));

    let doc = open_labels(true);
    assert!(matches!(doc.page_labels(), Err(EngineError::Panicked(m)) if m.contains("exploded")));
    assert_eq!(doc.panic_count(), 1);
}

#[test]
fn engines_without_labels_say_unsupported() {
    struct Plain;
    impl EngineDocument for Plain {
        fn page_count(&self) -> u32 {
            1
        }
        fn page_info(&self, _page: PageIndex) -> Result<PageInfo, EngineError> {
            Err(EngineError::Unsupported("test".into()))
        }
        fn render(
            &self,
            _request: &RenderRequest,
            _target: &mut PixmapMut<'_>,
            _cancel: &CancelToken,
        ) -> Result<RenderOutcome, EngineError> {
            Ok(RenderOutcome::default())
        }
    }
    assert!(matches!(
        Plain.page_labels(),
        Err(EngineError::Unsupported(_))
    ));
}
