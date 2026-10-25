use std::path::{Path, PathBuf};

use fastpdf_engine_api::{PageIndex, Rotation};

use super::format::{self, MAX_PATH_CHARS};
use super::*;

/// A unique, empty scratch directory under the system temp directory.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "fastpdf-positions-test-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn view(page: u32, offset: f32, zoom: SavedZoom, rotation: Rotation) -> SavedView {
    SavedView {
        page: PageIndex::new(page),
        offset,
        x: 0.0,
        zoom,
        rotation,
    }
}

fn record(page: u32) -> PositionRecord {
    PositionRecord {
        view: view(page, 0.25, SavedZoom::FitWidth, Rotation::R0),
        page_count: 2000,
        identity: Some(FileIdentity {
            len: 1_234_567,
            modified: 1_760_000_000_000_000_000,
        }),
    }
}

#[test]
fn records_round_trip_through_text() {
    let zooms = [
        SavedZoom::FitWidth,
        SavedZoom::FitPage,
        SavedZoom::Custom(2.5),
        SavedZoom::Custom(0.3333),
    ];
    let rotations = [Rotation::R0, Rotation::R90, Rotation::R180, Rotation::R270];
    let mut entries = Vec::new();
    for (i, zoom) in zooms.into_iter().enumerate() {
        for (j, rotation) in rotations.into_iter().enumerate() {
            let n = (i * 4 + j) as u32;
            let path = PathBuf::from(format!(r"C:\文件\閱讀 資料 {n}\書 {n}.pdf"));
            let record = PositionRecord {
                view: quantized(SavedView {
                    x: 0.125 * j as f32,
                    ..view(n * 7, -0.0125 + 0.1 * i as f32, zoom, rotation)
                }),
                page_count: 200,
                identity: (!n.is_multiple_of(3)).then_some(FileIdentity {
                    len: u64::from(n) * 1000 + 1,
                    modified: u64::MAX - u64::from(n),
                }),
            };
            entries.push((path, record));
        }
    }
    let text = format::to_text(&entries).0;
    assert!(text.starts_with("# FastPDF reading positions v1"));
    assert_eq!(format::parse(&text), entries);
    assert!(text.contains("\t2.5\t90\t"), "{text}");
    assert!(text.contains("\n1\t-0.0125\t0\tfit-width\t0\t200\t-\t-\t"));
}

#[test]
fn bad_lines_are_skipped_one_by_one() {
    let good = "12\t0.5\t0\tfit-page\t180\t40\t-\t-\tC:\\good.pdf";
    let bad = [
        "12\t0.5\t0\tfit-page\t180\t40\t-\tC:\\few.pdf",
        "12\t0.5\t0\tfit-page\t180\t40\t-\t-\tC:\\x.pdf\textra",
        "12\tNaN\t0\tfit-page\t180\t40\t-\t-\tC:\\nan.pdf",
        "12\t0.5\tinf\tfit-page\t180\t40\t-\t-\tC:\\inf.pdf",
        "12\t2.5\t0\tfit-page\t180\t40\t-\t-\tC:\\far.pdf",
        "0\t0.5\t0\tfit-page\t180\t40\t-\t-\tC:\\zero.pdf",
        "41\t0.5\t0\tfit-page\t180\t40\t-\t-\tC:\\past.pdf",
        "12\t0.5\t0\tfit-page\t45\t40\t-\t-\tC:\\tilted.pdf",
        "12\t0.5\t0\tbig\t0\t40\t-\t-\tC:\\big.pdf",
        "12\t0.5\t0\t-2\t0\t40\t-\t-\tC:\\negative.pdf",
        "12\t0.5\t0\t0\t0\t40\t-\t-\tC:\\nothing.pdf",
        "12\t0.5\t0\tfit-page\t0\t40\t5\t-\tC:\\half.pdf",
        "12\t0.5\t0\tfit-page\t0\t40\t-\t-\t",
        "12\t0.5\t0\tfit-page\t0\t40\t-\t-\tC:\\a\u{1}b.pdf",
    ];
    let long = format!(
        "12\t0.5\t0\tfit-page\t0\t40\t-\t-\tC:\\{}.pdf",
        "a".repeat(MAX_PATH_CHARS)
    );
    let mut text = String::from("\u{feff}# comment\r\n\r\n");
    for line in bad.iter().copied().chain([long.as_str()]) {
        text.push_str(line);
        text.push_str("\r\n");
    }
    text.push_str(good);
    text.push_str("\r\n7\t0\t0.5\t1.5\t90\t9\t10\t20\tC:\\last.pdf");
    let entries = format::parse(&text);
    let names: Vec<_> = entries.iter().map(|(p, _)| p.to_str().unwrap()).collect();
    assert_eq!(names, [r"C:\good.pdf", r"C:\last.pdf"]);
    let (_, last) = &entries[1];
    assert_eq!(
        last.view,
        SavedView {
            x: 0.5,
            ..view(6, 0.0, SavedZoom::Custom(1.5), Rotation::R90)
        }
    );
    assert_eq!(
        last.identity,
        Some(FileIdentity {
            len: 10,
            modified: 20
        })
    );
    // A zoom past the range is clamped when read, like every saved view.
    let huge = format::parse("1\t0\t0\t1e9\t0\t1\t-\t-\tC:\\a.pdf");
    assert_eq!(huge[0].1.view.zoom, SavedZoom::Custom(32.0));
}

#[test]
fn newest_first_and_bounded() {
    let dir = scratch("bounded");
    let store = PositionStore::at(dir.join("positions.txt"));
    for n in 0..=MAX_ENTRIES as u32 {
        store.remember(Path::new(&format!(r"C:\docs\{n}.pdf")), record(n));
        if n % 10 == 0 {
            store.flush().unwrap();
        }
    }
    store.flush().unwrap();
    let text = std::fs::read_to_string(store.path()).unwrap();
    let entries = format::parse(&text);
    assert_eq!(entries.len(), MAX_ENTRIES);
    assert_eq!(entries[0].0, Path::new(r"C:\docs\200.pdf"));
    assert_eq!(entries[MAX_ENTRIES - 1].0, Path::new(r"C:\docs\1.pdf"));
    assert_eq!(
        store.lookup(Path::new(r"C:\docs\0.pdf")),
        None,
        "oldest dropped"
    );

    // Leaving a document again moves it to the front, once.
    store.remember(Path::new(r"C:\docs\50.pdf"), record(7));
    store.flush().unwrap();
    let entries = format::parse(&std::fs::read_to_string(store.path()).unwrap());
    assert_eq!(entries.len(), MAX_ENTRIES);
    assert_eq!(entries[0], (PathBuf::from(r"C:\docs\50.pdf"), record(7)));
    assert_eq!(
        entries
            .iter()
            .filter(|(p, _)| p == Path::new(r"C:\docs\50.pdf"))
            .count(),
        1
    );
    if cfg!(windows) {
        // Windows paths are case-insensitive: the same document.
        store.remember(Path::new(r"c:\DOCS\50.PDF"), record(9));
        store.flush().unwrap();
        let entries = format::parse(&std::fs::read_to_string(store.path()).unwrap());
        assert_eq!(entries[0], (PathBuf::from(r"c:\DOCS\50.PDF"), record(9)));
        assert!(
            !entries[1..]
                .iter()
                .any(|(p, _)| same_path(p, &entries[0].0))
        );
        assert_eq!(store.lookup(Path::new(r"C:\Docs\50.pdf")), Some(record(9)));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_file_never_outgrows_its_cap() {
    // 200 paths of 2,048 CJK characters (6 KiB each) would make 1.2 MiB.
    let long = |n: usize| PathBuf::from(format!("{n:04}{}", "書".repeat(MAX_PATH_CHARS - 4)));
    let entries: Vec<_> = (0..MAX_ENTRIES).map(|n| (long(n), record(1))).collect();
    let (text, lines) = format::to_text(&entries);
    assert!(text.len() as u64 <= MAX_FILE_BYTES, "{}", text.len());
    let kept = format::parse(&text);
    assert!(kept.len() > 50, "{}", kept.len());
    assert_eq!(lines, kept.len(), "the count says what was written");
    // The newest are kept, in order.
    assert_eq!(kept[..], entries[..kept.len()]);
    // Paths that would not read back are never written.
    let odd = [
        PathBuf::from("C:\\tab\there.pdf"),
        PathBuf::from("C:\\line\nbreak.pdf"),
        PathBuf::from(""),
        PathBuf::from(format!("C:\\{}", "a".repeat(MAX_PATH_CHARS))),
    ];
    let odd: Vec<_> = odd.into_iter().map(|p| (p, record(1))).collect();
    assert_eq!(format::to_text(&odd).1, 0);
    assert!(format::parse(&format::to_text(&odd).0).is_empty());
}

#[test]
fn only_paths_the_file_can_hold_are_remembered() {
    let dir = scratch("unwritable");
    let store = PositionStore::at(dir.join("positions.txt"));
    let long = PathBuf::from(format!("C:\\{}.pdf", "a".repeat(MAX_PATH_CHARS)));
    assert!(!store.remember(&long, record(1)));
    assert!(!store.remember(Path::new("C:\\tab\there.pdf"), record(1)));
    assert_eq!(store.lookup(&long), None);
    assert_eq!(store.flush().unwrap(), 0, "nothing waited, nothing written");
    assert!(!store.path().exists());
    // A real U+FFFD is a character like any other (valid UTF-8): kept.
    let replacement = Path::new("C:\\docs\\報告\u{fffd}.pdf");
    assert!(store.remember(replacement, record(2)));
    assert!(store.remember(Path::new("C:\\docs\\b.pdf"), record(3)));
    assert_eq!(store.flush().unwrap(), 2);
    let fresh = PositionStore::at(dir.join("positions.txt"));
    assert_eq!(fresh.lookup(replacement), Some(record(2)));
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(windows)]
#[test]
fn paths_that_are_not_unicode_never_match_a_stored_one() {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    let dir = scratch("surrogate");
    let store = PositionStore::at(dir.join("positions.txt"));
    let stored = Path::new("C:\\docs\\x\u{fffd}.pdf");
    assert!(store.remember(stored, record(4)));
    // The same name with an unpaired surrogate where the U+FFFD is: shown
    // (lossily) the same, but another file.
    let mut wide: Vec<u16> = "C:\\docs\\x".encode_utf16().collect();
    wide.push(0xD800);
    wide.extend(".pdf".encode_utf16());
    let lone = PathBuf::from(OsString::from_wide(&wide));
    assert_eq!(lone.to_string_lossy(), stored.to_string_lossy());
    assert_eq!(store.lookup(&lone), None, "pending");
    store.flush().unwrap();
    assert_eq!(store.lookup(&lone), None, "written");
    assert_eq!(store.lookup(stored), Some(record(4)));
    assert!(!store.remember(&lone, record(5)));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_bounded_flush_gives_up_on_a_held_lock_and_keeps_the_records() {
    let dir = scratch("bounded-flush");
    let store = PositionStore::at(dir.join("positions.txt"));
    store.remember(Path::new("C:\\a.pdf"), record(1));
    // Another process writing the file holds the shared lock.
    let held = crate::paths::lock_beside(store.path(), std::time::Duration::ZERO).unwrap();
    let started = std::time::Instant::now();
    let err = store
        .flush_within(std::time::Duration::from_millis(50))
        .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);
    assert!(started.elapsed() < std::time::Duration::from_millis(400));
    assert!(!store.path().exists());
    drop(held);
    // Still queued: the next flush writes it.
    assert_eq!(store.flush_within(std::time::Duration::ZERO).unwrap(), 1);
    assert_eq!(
        PositionStore::at(dir.join("positions.txt")).lookup(Path::new("C:\\a.pdf")),
        Some(record(1))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_flush_removes_temporary_files_an_interrupted_write_left() {
    let dir = scratch("stale");
    let store = PositionStore::next_to(&dir.join("recent.txt"));
    let stale = dir.join("recent-positions.txt.4242.7.tmp");
    std::fs::write(&stale, "half").unwrap();
    store.remember(Path::new("C:\\a.pdf"), record(1));
    assert_eq!(store.flush().unwrap(), 1);
    assert!(!stale.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_oversized_directory_and_non_utf8_files_read_as_empty() {
    let dir = scratch("odd-files");
    let doc = Path::new(r"C:\a.pdf");
    let missing = PositionStore::at(dir.join("missing.txt"));
    assert_eq!(missing.lookup(doc), None);

    let good_line = "3\t0\t0\tfit-width\t0\t9\t-\t-\tC:\\a.pdf\n";
    let large = dir.join("large.txt");
    let mut text = good_line.to_string();
    text.push_str(&"#".repeat(MAX_FILE_BYTES as usize));
    std::fs::write(&large, text).unwrap();
    assert_eq!(PositionStore::at(large.clone()).lookup(doc), None);
    let binary = dir.join("binary.txt");
    let mut bytes = good_line.as_bytes().to_vec();
    bytes.extend_from_slice(&[0xff, 0xfe, 0x00, b'\n']);
    std::fs::write(&binary, bytes).unwrap();
    assert_eq!(PositionStore::at(binary).lookup(doc), None);
    let small = dir.join("small.txt");
    std::fs::write(&small, good_line).unwrap();
    assert!(PositionStore::at(small).lookup(doc).is_some());

    // A directory where the file should be: nothing found, nothing written.
    let folder = PositionStore::at(dir.join("folder"));
    std::fs::create_dir(folder.path()).unwrap();
    assert_eq!(folder.lookup(doc), None);
    folder.remember(doc, record(1));
    assert!(folder.flush().is_err());
    assert!(folder.path().is_dir());
    // An oversized file is not FastPDF's: the next write replaces it.
    let replaced = PositionStore::at(large.clone());
    replaced.remember(doc, record(4));
    replaced.flush().unwrap();
    assert!(std::fs::metadata(&large).unwrap().len() < 1024);
    assert_eq!(
        replaced.lookup(Path::new(r"C:\A.pdf")).is_some(),
        cfg!(windows)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn two_stores_on_one_file_merge() {
    // Two FastPDF processes keep one file: each flush keeps the other's.
    let dir = scratch("merge");
    let path = dir.join("recent-positions.txt");
    let first = PositionStore::at(path.clone());
    let second = PositionStore::at(path.clone());
    first.remember(Path::new(r"C:\one.pdf"), record(1));
    second.remember(Path::new(r"C:\two.pdf"), record(2));
    first.flush().unwrap();
    second.flush().unwrap();
    let fresh = PositionStore::at(path);
    assert_eq!(fresh.lookup(Path::new(r"C:\one.pdf")), Some(record(1)));
    assert_eq!(fresh.lookup(Path::new(r"C:\two.pdf")), Some(record(2)));
    // The newer flush comes first.
    let text = std::fs::read_to_string(fresh.path()).unwrap();
    let names: Vec<_> = format::parse(&text).into_iter().map(|(p, _)| p).collect();
    assert_eq!(
        names,
        [PathBuf::from(r"C:\two.pdf"), PathBuf::from(r"C:\one.pdf")]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn concurrent_flushes_of_one_file_lose_nothing() {
    // Each store stands for a FastPDF process: the shared lock makes their
    // read, merge and replace one after another.
    let dir = scratch("concurrent");
    let path = dir.join("recent-positions.txt");
    let writers: Vec<_> = (0..8u32)
        .map(|n| {
            let path = path.clone();
            std::thread::spawn(move || {
                let store = PositionStore::at(path);
                for round in 0..5 {
                    let doc = format!(r"C:\writer{n}\doc{round}.pdf");
                    store.remember(Path::new(&doc), record(n));
                    // A flush that waited too long leaves its records for
                    // the next one, like the reader does.
                    while let Err(e) = store.flush() {
                        assert_eq!(e.kind(), std::io::ErrorKind::WouldBlock);
                    }
                }
            })
        })
        .collect();
    for writer in writers {
        writer.join().unwrap();
    }
    let entries = format::parse(&std::fs::read_to_string(&path).unwrap());
    assert_eq!(entries.len(), 40);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pending_records_answer_lookups_before_the_flush() {
    let dir = scratch("pending");
    let store = PositionStore::at(dir.join("positions.txt"));
    let doc = Path::new(r"C:\book.pdf");
    store.remember(doc, record(10));
    store.flush().unwrap();
    store.remember(doc, record(20));
    // Not written yet, still found: a document closed and opened again at
    // once starts where it was left.
    assert_eq!(store.lookup(doc), Some(record(20)));
    assert_eq!(
        PositionStore::at(dir.join("positions.txt")).lookup(doc),
        Some(record(10))
    );
    // Pending records are bounded; the newest stay.
    for n in 0..40 {
        store.remember(Path::new(&format!(r"C:\other{n}.pdf")), record(n));
    }
    assert_eq!(
        store.lookup(doc),
        Some(record(10)),
        "dropped from pending, read from the file"
    );
    store.flush().unwrap();
    assert_eq!(store.lookup(Path::new(r"C:\other39.pdf")), Some(record(39)));
    assert_eq!(store.lookup(Path::new(r"C:\other0.pdf")), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_temporary_file_left_behind() {
    let dir = scratch("temp");
    let store = PositionStore::next_to(&dir.join("recent.txt"));
    assert_eq!(store.flush().unwrap(), 0);
    assert!(
        !store.path().exists(),
        "nothing remembered, nothing written"
    );
    store.remember(Path::new(r"C:\a.pdf"), record(3));
    assert_eq!(store.flush().unwrap(), 1);
    store.remember(Path::new(r"C:\b.pdf"), record(4));
    store.flush().unwrap();
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    // The lock file stays (empty): removing it would let two processes
    // lock different files.
    assert_eq!(names, ["recent-positions.txt", "recent-positions.txt.lock"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn next_to_derives_a_sibling() {
    let store = PositionStore::next_to(Path::new(r"C:\data\FastPDF\recent.txt"));
    assert_eq!(
        store.path(),
        Path::new(r"C:\data\FastPDF\recent-positions.txt")
    );
    let custom = PositionStore::next_to(Path::new(r"D:\scratch\gui\list.lst"));
    assert_eq!(
        custom.path(),
        Path::new(r"D:\scratch\gui\list-positions.txt")
    );
    let bare = PositionStore::next_to(Path::new("history"));
    assert_eq!(bare.path(), Path::new("history-positions.txt"));
}

#[test]
fn restore_rules() {
    let saved = record(30);
    let identity = saved.identity;
    let exact = restore_for(&saved, 2000, identity).unwrap();
    assert_eq!(exact, Restore::Exact(saved.view));
    assert_eq!(exact.scope(), RestoreScope::Exact);
    assert_eq!(exact.kind(), "exact");

    let touched = identity.map(|id| FileIdentity {
        modified: id.modified + 1,
        ..id
    });
    let grown = identity.map(|id| FileIdentity {
        len: id.len + 1,
        ..id
    });
    for now in [touched, grown, None] {
        let place = restore_for(&saved, 2000, now).unwrap();
        assert_eq!(place, Restore::Place(saved.view), "{now:?}");
        assert_eq!(place.scope(), RestoreScope::PlaceOnly);
    }
    // Same file identity but another page count: the document changed.
    assert_eq!(
        restore_for(&saved, 31, identity),
        Some(Restore::Place(saved.view))
    );
    // The page is gone.
    assert_eq!(restore_for(&saved, 30, identity), None);
    assert_eq!(restore_for(&saved, 0, identity), None);
    // Unknown when saved: never exact.
    let unknown = PositionRecord {
        identity: None,
        ..saved
    };
    assert_eq!(
        restore_for(&unknown, 2000, None),
        Some(Restore::Place(saved.view))
    );
    // Applied views are sanitized.
    let wild = PositionRecord {
        view: SavedView {
            offset: f32::NAN,
            ..saved.view
        },
        ..saved
    };
    assert_eq!(
        restore_for(&wild, 2000, identity).unwrap().view().offset,
        0.0
    );
}

#[test]
fn file_identity_follows_the_file() {
    let dir = scratch("identity");
    let path = dir.join("doc.pdf");
    assert_eq!(FileIdentity::of(&path), None);
    std::fs::write(&path, b"%PDF-1.7").unwrap();
    let first = FileIdentity::of(&path).unwrap();
    assert_eq!(first.len, 8);
    assert!(first.modified > 0);
    std::fs::write(&path, b"%PDF-1.7 changed").unwrap();
    assert_ne!(FileIdentity::of(&path), Some(first));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn views_are_kept_at_four_decimals() {
    let fine = SavedView {
        x: 0.123_456,
        ..view(4, 0.250_04, SavedZoom::Custom(1.333_333), Rotation::R0)
    };
    let kept = quantized(fine);
    assert_eq!((kept.offset, kept.x), (0.25, 0.1235));
    assert_eq!(kept.zoom, SavedZoom::Custom(1.3333));
    assert_eq!(quantized(kept), kept);
    let entries = vec![(
        PathBuf::from(r"C:\q.pdf"),
        PositionRecord {
            view: fine,
            ..record(4)
        },
    )];
    assert_eq!(format::parse(&format::to_text(&entries).0)[0].1.view, kept);
}
