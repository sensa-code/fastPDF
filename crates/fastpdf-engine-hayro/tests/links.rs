//! Link annotations: flags, actions FastPDF never runs, rectangles, URI
//! caps, and the budgets that keep hostile pages (floods, cyclic or huge
//! name trees, shared huge objects) bounded. Also checks the generated
//! link fixtures when present.

mod common;

use std::time::{Duration, Instant};

use common::{PageSpec, build_full, fixture, open, page_object};
use fastpdf_engine_api::{DestinationView, EngineDocument, Link, LinkTarget, PageIndex};

/// Two Letter pages; page 1 gets `page_extra`, the catalog `catalog_extra`,
/// and `objects` are placed at fixed object numbers (100 and up).
fn document(page_extra: &str, catalog_extra: &str, objects: &[(u32, String)]) -> Vec<u8> {
    let mut pages = [
        PageSpec::new([0.0, 0.0, 612.0, 792.0], ""),
        PageSpec::new([0.0, 0.0, 612.0, 792.0], ""),
    ];
    pages[0].extra = page_extra.to_owned();
    build_full(&pages, catalog_extra, |b| {
        let last = objects.iter().map(|(n, _)| *n).max().unwrap_or(0);
        while b.reserve() < last {}
        for (n, body) in objects {
            b.set(*n, body.clone());
        }
    })
}

fn link(rect: &str, target: &str) -> String {
    format!("<< /Type /Annot /Subtype /Link /Rect [{rect}] {target} >>")
}

fn annots(numbers: impl IntoIterator<Item = u32>) -> String {
    let refs: Vec<String> = numbers.into_iter().map(|n| format!("{n} 0 R")).collect();
    format!("/Annots [{}]", refs.join(" "))
}

fn links_of(bytes: Vec<u8>) -> Vec<Link> {
    match open(bytes).links(PageIndex::FIRST) {
        Ok(links) => links,
        Err(e) => panic!("links failed: {e:?}"),
    }
}

fn uri_of(len: usize) -> String {
    format!("https://example.invalid/{}", "a".repeat(len - 24))
}

#[test]
fn links_flags_actions_and_rects() {
    let p1 = page_object(1);
    let objects = [
        (
            100,
            link("72 700 172 720", &format!("/F 2 /Dest [{p1} 0 R /Fit]")),
        ),
        (
            101,
            link("72 650 172 670", &format!("/F 32 /Dest [{p1} 0 R /Fit]")),
        ),
        (
            102,
            link("72 600 172 620", &format!("/F 4 /Dest [{p1} 0 R /Fit]")),
        ),
        (
            103,
            link(
                "172 520 72 500",
                "/A << /S /URI /URI (https://example.invalid/) >>",
            ),
        ),
        (
            104,
            link("72 400 172 420", "/A << /S /Launch /F (calc.exe) >>"),
        ),
        (
            105,
            link(
                "72 350 172 370",
                "/A << /S /GoToR /F (o.pdf) /D [0 /Fit] >>",
            ),
        ),
        (
            106,
            link(
                "72 300 172 320",
                "/A << /S /JavaScript /JS (app.alert(1)) >>",
            ),
        ),
        (
            107,
            link(
                "72 250 172 270",
                &format!("/A << /S /URI /URI ({}) >>", uri_of(9 * 1024)),
            ),
        ),
        (
            108,
            link(
                "72 200 172 220",
                &format!("/A << /S /URI /URI ({}) >>", uri_of(8 * 1024)),
            ),
        ),
    ];
    // 102 is listed twice: one link.
    let bytes = document(
        &annots([100, 101, 102, 102, 103, 104, 105, 106, 107, 108]),
        "",
        &objects,
    );
    let links = links_of(bytes);
    assert_eq!(links.len(), 7, "{links:#?}");
    // Hidden (/F 2) and NoView (/F 32) are left out; Print (/F 4) is kept.
    assert!(matches!(&links[0].target, LinkTarget::Internal(d) if d.page == PageIndex::new(1)));
    // An inverted /Rect is normalized (page space: y down from the top).
    let b = links[1].bounds;
    assert_eq!((b.x0, b.y0, b.x1, b.y1), (72.0, 272.0, 172.0, 292.0));
    assert_eq!(
        links[1].target,
        LinkTarget::Uri("https://example.invalid/".into())
    );
    // Launch, GoToR and JavaScript are never run.
    for link in &links[2..5] {
        assert_eq!(link.target, LinkTarget::Unsupported);
    }
    // URIs over 8 KiB are not passed on.
    assert_eq!(links[5].target, LinkTarget::Unsupported);
    assert!(matches!(&links[6].target, LinkTarget::Uri(u) if u.len() == 8 * 1024));
}

#[test]
fn named_destinations_resolve_for_every_link() {
    let p1 = page_object(1);
    // 500 links share one name in a /Kids tree with /Limits; 1500 more use
    // distinct names of a flat leaf of 3000 names without /Limits.
    let mut objects = vec![
        (100, "<< /Kids [101 0 R 102 0 R] >>".to_owned()),
        (
            101,
            format!("<< /Limits [(a) (m)] /Names [(chapter) [{p1} 0 R /FitH 500]] >>"),
        ),
        (102, "<< /Limits [(n) (z)] /Kids [103 0 R] >>".to_owned()),
    ];
    let flat: Vec<String> = (0..3000)
        .map(|i| format!("(n{i:04}) [{p1} 0 R /XYZ 0 {} 0]", 792 - i % 700))
        .collect();
    objects.push((103, format!("<< /Names [{}] >>", flat.join(" "))));
    let first = 200;
    for i in 0..2000u32 {
        let dest = if i < 500 {
            "(chapter)".to_owned()
        } else {
            format!("(n{:04})", (i - 500) * 2)
        };
        let x = (i % 50) * 10;
        let y = (i / 50) * 10;
        objects.push((
            first + i,
            link(
                &format!("{x} {y} {} {}", x + 8, y + 8),
                &format!("/Dest {dest}"),
            ),
        ));
    }
    let bytes = document(
        &annots(first..first + 2000),
        "/Names << /Dests 100 0 R >>",
        &objects,
    );
    let started = Instant::now();
    let links = links_of(bytes);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(links.len(), 2000);
    for (i, link) in links.iter().enumerate() {
        let LinkTarget::Internal(d) = &link.target else {
            panic!("link {i}: {:?}", link.target);
        };
        assert_eq!(d.page, PageIndex::new(1), "link {i}");
        let expected = if i < 500 {
            DestinationView::FitWidth { top: Some(292.0) }
        } else {
            let n = (i as u32 - 500) * 2;
            DestinationView::Xyz {
                left: Some(0.0),
                top: Some((n % 700) as f32),
                zoom: None,
            }
        };
        assert_eq!(d.view, expected, "link {i}");
    }
}

#[test]
fn limits_keep_unneeded_leaves_unread() {
    // Four leaves of 50,000 names each (more than one call reads in total);
    // the lookups all fall into the first leaf, which a lookup reaches last:
    // the leaves before it are passed by their /Limits without being read.
    let p1 = page_object(1);
    let mut objects = vec![(
        104,
        "<< /Kids [100 0 R 101 0 R 102 0 R 103 0 R] >>".to_owned(),
    )];
    for (n, prefix) in (100..104u32).zip(['a', 'b', 'c', 'd']) {
        let names: Vec<String> = (0..50_000)
            .map(|i| format!("({prefix}{i:05}) [{p1} 0 R /Fit]"))
            .collect();
        objects.push((
            n,
            format!(
                "<< /Limits [({prefix}00000) ({prefix}49999)] /Names [{}] >>",
                names.join(" ")
            ),
        ));
    }
    let first = 200;
    for i in 0..500u32 {
        objects.push((
            first + i,
            link("0 0 10 10", &format!("/Dest (a{:05})", i * 97)),
        ));
    }
    let bytes = document(
        &annots(first..first + 500),
        "/Names << /Dests 104 0 R >>",
        &objects,
    );
    let links = links_of(bytes);
    assert_eq!(links.len(), 500);
    assert!(
        links
            .iter()
            .all(|l| matches!(&l.target, LinkTarget::Internal(d) if d.page == PageIndex::new(1)))
    );
}

#[test]
fn hostile_name_trees_and_floods_stay_bounded() {
    // A cycle, a 300-level chain and a flat leaf of 100,000 names without
    // /Limits; 10,500 links look up names that are not there, some links
    // share one action whose URI is a 10 KiB string.
    let mut objects = vec![
        (100, "<< /Kids [101 0 R 102 0 R 400 0 R] >>".to_owned()),
        (101, "<< /Kids [100 0 R 101 0 R] >>".to_owned()),
    ];
    for depth in 0..300u32 {
        let body = if depth + 1 < 300 {
            format!("<< /Kids [{} 0 R] >>", 103 + depth)
        } else {
            "<< /Names [(deep) [5 0 R /Fit]] >>".to_owned()
        };
        objects.push((102 + depth, body));
    }
    let flat: Vec<String> = (0..100_000).map(|i| format!("(x{i}) 5 0 R")).collect();
    objects.push((400, format!("<< /Names [{}] >>", flat.join(" "))));
    objects.push((401, format!("({})", uri_of(10 * 1024))));
    objects.push((402, "<< /S /URI /URI 401 0 R >>".to_owned()));
    let first = 1000;
    let count = 10_500u32;
    for i in 0..count {
        let target = if i % 3 == 0 {
            "/A 402 0 R".to_owned()
        } else {
            format!("/Dest (missing-{i})")
        };
        objects.push((first + i, link("0 0 10 10", &target)));
    }
    // An outline of 2000 bookmarks to missing names uses the same budgets.
    let items = 2000u32;
    let outline = 20_000;
    objects.push((
        403,
        format!(
            "<< /First {outline} 0 R /Last {} 0 R /Count {items} >>",
            outline + items - 1
        ),
    ));
    for i in 0..items {
        let next = if i + 1 < items {
            format!("/Next {} 0 R", outline + i + 1)
        } else {
            String::new()
        };
        objects.push((
            outline + i,
            format!("<< /Title (b{i}) /Parent 403 0 R {next} /Dest (gone-{i}) >>"),
        ));
    }
    let bytes = document(
        &annots(first..first + count),
        "/Names << /Dests 100 0 R >> /Outlines 403 0 R",
        &objects,
    );
    let doc = open(bytes);
    let started = Instant::now();
    let links = match doc.links(PageIndex::FIRST) {
        Ok(links) => links,
        Err(e) => panic!("{e:?}"),
    };
    let took = started.elapsed();
    assert!(took < Duration::from_secs(20), "links took {took:?}");
    // At most 10,000 links per page, none of them followed anywhere.
    assert_eq!(links.len(), 10_000);
    assert!(links.iter().all(|l| l.target == LinkTarget::Unsupported));
    let started = Instant::now();
    let outline = match doc.outline() {
        Ok(outline) => outline,
        Err(e) => panic!("{e:?}"),
    };
    let took = started.elapsed();
    assert!(took < Duration::from_secs(20), "outline took {took:?}");
    assert_eq!(outline.len(), 2000);
    assert!(outline.iter().all(|item| item.destination.is_none()));
}

#[test]
fn uri_bytes_per_call_are_capped() {
    // 300 links with distinct 7 KiB URIs: 2.1 MB in total, over the 1 MiB
    // a call returns.
    let mut objects = Vec::new();
    for i in 0..300u32 {
        objects.push((
            100 + i,
            link(
                "0 0 10 10",
                &format!("/A << /S /URI /URI ({}{i}) >>", uri_of(7 * 1024)),
            ),
        ));
    }
    let links = links_of(document(&annots(100..400), "", &objects));
    assert_eq!(links.len(), 300);
    let returned: usize = links
        .iter()
        .map(|l| match &l.target {
            LinkTarget::Uri(u) => u.len(),
            _ => 0,
        })
        .sum();
    assert!(returned <= 1 << 20, "{returned}");
    assert!(returned > 1 << 19, "{returned}");
    assert_eq!(
        links.last().map(|l| &l.target),
        Some(&LinkTarget::Unsupported)
    );
}

/// The link under page-space point (x, y): the topmost (last) one.
fn at(links: &[Link], x: f32, y: f32) -> Option<&LinkTarget> {
    links
        .iter()
        .rev()
        .find(|l| l.bounds.contains(x, y))
        .map(|l| &l.target)
}

fn page(target: Option<&LinkTarget>) -> Option<u32> {
    match target {
        Some(LinkTarget::Internal(d)) => Some(d.page.get()),
        _ => None,
    }
}

fn uri(target: Option<&LinkTarget>) -> Option<&str> {
    match target {
        Some(LinkTarget::Uri(u)) => Some(u),
        _ => None,
    }
}

#[test]
fn link_fixtures() {
    if let Some(bytes) = fixture("small-text/links-actions.pdf") {
        let doc = open(bytes);
        let links = match doc.links(PageIndex::FIRST) {
            Ok(links) => links,
            Err(e) => panic!("{e:?}"),
        };
        assert_eq!(page(at(&links, 100.0, 100.0)), Some(2));
        assert_eq!(
            uri(at(&links, 100.0, 160.0)),
            Some("https://example.invalid/path?q=1#frag")
        );
        assert_eq!(
            uri(at(&links, 100.0, 220.0)),
            Some("https://b\u{fc}cher.example/")
        );
        assert_eq!(
            uri(at(&links, 100.0, 280.0)),
            Some("file:///C:/Windows/System32/calc.exe")
        );
        assert_eq!(uri(at(&links, 100.0, 340.0)), Some("javascript:alert(1)"));
        for y in [400.0, 460.0, 520.0] {
            assert_eq!(
                at(&links, 100.0, y),
                Some(&LinkTarget::Unsupported),
                "y {y}"
            );
        }
        assert!(uri(at(&links, 100.0, 580.0)).is_some_and(|u| u.starts_with("mailto:")));
        assert!(matches!(
            at(&links, 100.0, 640.0),
            Some(LinkTarget::Internal(d)) if d.page.get() == 1
                && d.view == DestinationView::Xyz { left: Some(72.0), top: Some(40.0), zoom: None }
        ));
        assert!(matches!(
            at(&links, 100.0, 700.0),
            Some(LinkTarget::Internal(d)) if d.page.get() == 2
                && d.view == DestinationView::FitWidth { top: Some(0.0) }
        ));
        // Hidden and NoView links are not there.
        assert_eq!(at(&links, 350.0, 100.0), None);
        assert_eq!(at(&links, 350.0, 640.0), None);
        // Overlapping links: the later one is on top.
        assert_eq!(page(at(&links, 350.0, 160.0)), Some(2));
        assert_eq!(page(at(&links, 450.0, 160.0)), Some(1));
        assert_eq!(page(at(&links, 350.0, 220.0)), Some(1));
        assert_eq!(at(&links, 350.0, 400.0), Some(&LinkTarget::Unsupported));
        assert_eq!(uri(at(&links, 350.0, 580.0)).map(str::len), Some(3000));
        // The rotated page's back link, in unrotated page space.
        let back = match doc.links(PageIndex::new(2)) {
            Ok(links) => links,
            Err(e) => panic!("{e:?}"),
        };
        assert_eq!(page(at(&back, 100.0, 100.0)), Some(0));
    }
    if let Some(bytes) = fixture("malformed/links-flood.pdf") {
        let doc = open(bytes);
        let started = Instant::now();
        let links = match doc.links(PageIndex::FIRST) {
            Ok(links) => links,
            Err(e) => panic!("{e:?}"),
        };
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(links.len(), 10_000);
        assert!(
            links
                .iter()
                .all(|l| l.bounds.x0.is_finite() && l.bounds.y1.is_finite())
        );
    }
}

#[test]
fn unreadable_array_items_end_the_walk_without_a_panic() {
    // A dictionary followed by `stream` inside an array: hayro's own array
    // iterator panics on it.
    let bad = "<< /F 1 >> stream
abc
endstream";
    let p1 = page_object(1);
    let objects = [
        (100, link("0 0 10 10", "/Dest (one)")),
        (101, link("20 0 30 10", "/Dest (two)")),
        (
            102,
            link("40 0 50 10", &format!("/Dest [{p1} 0 R /XYZ {bad}]")),
        ),
        (
            103,
            format!("<< /Names [(one) [{p1} 0 R /Fit] {bad} (two) [{p1} 0 R /Fit]] >>"),
        ),
    ];
    let bytes = document(
        &format!("/Annots [100 0 R 102 0 R {bad} 101 0 R]"),
        "/Names << /Dests 103 0 R >>",
        &objects,
    );
    let links = links_of(bytes);
    assert_eq!(links.len(), 2, "{links:?}");
    // The names before the bad item resolve; the explicit destination
    // keeps what came before it.
    assert!(matches!(&links[0].target, LinkTarget::Internal(d) if d.page == PageIndex::new(1)));
    assert!(matches!(
        &links[1].target,
        LinkTarget::Internal(d) if d.view == DestinationView::Xyz { left: None, top: None, zoom: None }
    ));
}
