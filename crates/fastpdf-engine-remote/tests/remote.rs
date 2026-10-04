//! End-to-end tests: real render host processes (the test-only host with
//! the synthetic engine) behind `RemoteEngine`, compared with the same
//! engine in-process.

#![cfg(windows)]

#[path = "../test-host/synthetic.rs"]
mod synthetic;

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use fastpdf_engine_api::{
    CancelToken, ColorMode, DocumentSource, EngineDocument, EngineError, GuardedDocument,
    LimitKind, MemoryPressure, OpenOptions, OutlineItem, PageIndex, PdfEngine, PixelFormat,
    PixelRect, Pixmap, RenderOutcome, RenderRequest, RenderScale, Rgba8, Rotation, SharedBytes,
    open_guarded,
};
use fastpdf_engine_remote::{CrashPolicy, HostStats, RemoteConfig, RemoteEngine};
use synthetic::{Behavior, Spec, SyntheticEngine};

const HOST: &str = env!("CARGO_BIN_EXE_fastpdf-remote-test-host");

fn config() -> RemoteConfig {
    let mut c = RemoteConfig::new(HOST, synthetic::NAME);
    c.args = vec!["--render-host".into()];
    c.memory_limit = Some(256 << 20);
    c.host_workers = 4;
    c
}

/// No crash storm in tests that crash on purpose more than twice.
fn patient(mut c: RemoteConfig) -> RemoteConfig {
    c.crash_policy = CrashPolicy {
        storm_crashes: 100,
        max_total_crashes: None,
        ..CrashPolicy::default()
    };
    c
}

fn engine(c: RemoteConfig) -> RemoteEngine {
    RemoteEngine::new(c).unwrap_or_else(|e| panic!("render host did not start: {e}"))
}

fn open_with(
    engine: &dyn PdfEngine,
    spec: &Spec,
    password: Option<&str>,
) -> Result<GuardedDocument, EngineError> {
    let source =
        DocumentSource::from_bytes(SharedBytes::from_vec(spec.bytes())).with_path("synthetic.pdf");
    let options = OpenOptions {
        password: password.map(str::to_owned),
        ..OpenOptions::default()
    };
    open_guarded(engine, source, &options)
}

fn open(engine: &dyn PdfEngine, spec: &Spec) -> GuardedDocument {
    open_with(engine, spec, None).unwrap_or_else(|e| panic!("open failed: {e}"))
}

fn scale(s: f32) -> RenderScale {
    RenderScale::new(s).unwrap_or_else(|| panic!("bad scale {s}"))
}

fn full_request(doc: &GuardedDocument, page: u32, s: f32, rotation: Rotation) -> RenderRequest {
    let info = doc
        .page_info(PageIndex::new(page))
        .unwrap_or_else(|e| panic!("page info: {e}"));
    RenderRequest::full_page(
        PageIndex::new(page),
        info.size,
        info.rotation,
        rotation,
        scale(s),
    )
}

fn render_with(
    doc: &GuardedDocument,
    request: &RenderRequest,
    format: PixelFormat,
    cancel: &CancelToken,
) -> Result<(Pixmap, RenderOutcome), EngineError> {
    let mut pixmap = Pixmap::new(request.region.size(), format, doc.limits())?;
    let outcome = doc.render(request, &mut pixmap.as_mut(), cancel)?;
    Ok((pixmap, outcome))
}

fn render(doc: &GuardedDocument, page: u32) -> Result<Pixmap, EngineError> {
    let request = full_request(doc, page, 0.5, Rotation::R0);
    render_with(doc, &request, PixelFormat::default(), &CancelToken::new()).map(|(p, _)| p)
}

fn stats(engine: &RemoteEngine) -> HostStats {
    engine.host_stats().into_iter().next().unwrap_or_default()
}

fn outline_depth(items: &[OutlineItem]) -> usize {
    let mut depth = 0;
    let mut level = items;
    while let Some(first) = level.first() {
        depth += 1;
        level = &first.children;
    }
    depth
}

fn poll<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "timed out: {what}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn remote_results_are_identical_to_in_process() {
    let spec = Spec::new(8);
    let local = open(&SyntheticEngine, &spec);
    let engine = engine(config());
    assert_eq!(engine.info(), SyntheticEngine.info());
    let remote = open(&engine, &spec);
    assert_eq!(remote.page_count(), local.page_count());
    let cancel = CancelToken::new();
    let paper = Rgba8::new(250, 240, 200, 255);
    for page in 0..8 {
        let p = PageIndex::new(page);
        assert_eq!(remote.page_info(p), local.page_info(p));
        // Slot-sized and larger-than-slot renders (scale 4 is ~5 MB), both
        // pixel formats, rotations, night mode, a paper color and a tile.
        for (s, rotation, format, color) in [
            (
                0.25,
                Rotation::R0,
                PixelFormat::Rgba8Premultiplied,
                ColorMode::Normal,
            ),
            (
                1.0,
                Rotation::R90,
                PixelFormat::Bgra8Premultiplied,
                ColorMode::Normal,
            ),
            (
                96.0 / 72.0,
                Rotation::R0,
                PixelFormat::Rgba8Premultiplied,
                ColorMode::Inverted,
            ),
            (
                4.0,
                Rotation::R270,
                PixelFormat::Bgra8Premultiplied,
                ColorMode::Normal,
            ),
        ] {
            let mut request = full_request(&local, page, s, rotation);
            request.color_mode = color;
            request.background = paper;
            let a = render_with(&local, &request, format, &cancel);
            let b = render_with(&remote, &request, format, &cancel);
            assert!(a.is_ok(), "page {page} scale {s}: {a:?}");
            assert_eq!(a, b, "page {page} scale {s} {rotation:?} {format:?}");
            if s >= 4.0 {
                let tile = request.with_region(PixelRect::new(256, 128, 512, 516));
                let a = render_with(&local, &tile, format, &cancel);
                assert!(a.is_ok());
                assert_eq!(a, render_with(&remote, &tile, format, &cancel));
            }
        }
        assert_eq!(remote.text_layer(p, &cancel), local.text_layer(p, &cancel));
        assert_eq!(remote.links(p), local.links(p));
    }
    assert_eq!(remote.outline(), local.outline());
    assert_eq!(remote.metadata(), local.metadata());
    assert_eq!(
        remote.page_info(PageIndex::new(99)),
        local.page_info(PageIndex::new(99))
    );
    let s = stats(&engine);
    assert!(
        s.pid.is_some() && s.private_bytes.is_some_and(|b| b > 0),
        "{s:?}"
    );
    assert_eq!((s.crashes, s.restarts), (0, 0));
}

#[test]
fn outlines_deeper_than_the_protocol_cap_are_cut() {
    let spec = Spec::new(2).deep_outline(200);
    let local = open(&SyntheticEngine, &spec);
    let engine = engine(config());
    let remote = open(&engine, &spec);
    assert_eq!(outline_depth(&local.outline().unwrap_or_default()), 200);
    assert_eq!(outline_depth(&remote.outline().unwrap_or_default()), 64);
}

#[test]
fn panics_stay_inside_the_host() {
    let engine = engine(config());
    let doc = open(&engine, &Spec::new(4).page(2, Behavior::Panic));
    let pid = stats(&engine).pid;
    let err = render(&doc, 2).expect_err("the panicking page fails");
    assert!(
        matches!(&err, EngineError::Panicked(m) if m.contains("synthetic panic")),
        "{err}"
    );
    let reference = open(&SyntheticEngine, &Spec::new(4));
    assert_eq!(render(&doc, 1), render(&reference, 1));
    let s = stats(&engine);
    assert_eq!(s.pid, pid, "a contained panic must not restart the host");
    assert_eq!((s.crashes, s.restarts), (0, 0));
}

/// The crashing page fails, the host is replaced, other pages still render.
fn crash_and_recover(behavior: Behavior, expect: &str) {
    let engine = engine(patient(config()));
    let doc = open(&engine, &Spec::new(4).page(1, behavior));
    let reference = open(&SyntheticEngine, &Spec::new(4));
    let first_pid = stats(&engine)
        .pid
        .unwrap_or_else(|| panic!("host not running"));
    let started = Instant::now();
    let Err(err) = render(&doc, 1) else {
        panic!("the crashing page rendered");
    };
    let detected = started.elapsed();
    assert!(
        matches!(&err, EngineError::Internal(m) if m.contains(expect)),
        "{behavior:?}: {err}"
    );
    let started = Instant::now();
    assert_eq!(render(&doc, 0), render(&reference, 0));
    let recovered = started.elapsed();
    let s = stats(&engine);
    assert_ne!(s.pid, Some(first_pid));
    assert_eq!((s.crashes, s.restarts), (1, 1), "{s:?}");
    assert!(s.last_crash.is_some_and(|c| c.contains(expect)));
    eprintln!("{behavior:?}: failure reported after {detected:?}, next page after {recovered:?}");
}

#[test]
fn abort_kills_only_the_host() {
    crash_and_recover(Behavior::Abort, "fail-fast");
}

#[test]
fn stack_overflow_kills_only_the_host() {
    crash_and_recover(Behavior::StackOverflow, "stack overflow");
}

#[test]
fn unexpected_exit_is_a_crash() {
    crash_and_recover(Behavior::Exit, "exit code 0x00000003");
}

#[test]
fn memory_limit_kills_only_the_host() {
    crash_and_recover(Behavior::Bomb, "memory limit (256 MiB)");
}

#[test]
fn hung_requests_hit_their_deadline() {
    let mut c = config();
    c.request_timeout = Some(Duration::from_secs(2));
    let engine = engine(c);
    let doc = open(&engine, &Spec::new(3).page(1, Behavior::Hang));
    let started = Instant::now();
    assert_eq!(
        render(&doc, 1).err(),
        Some(EngineError::LimitExceeded(LimitKind::RenderTime))
    );
    let took = started.elapsed();
    assert!(
        took >= Duration::from_secs(2) && took < Duration::from_secs(8),
        "deadline took {took:?}"
    );
    assert!(render(&doc, 0).is_ok());
    let s = stats(&engine);
    assert_eq!((s.crashes, s.restarts), (1, 1), "{s:?}");
    eprintln!("hang: reported after {took:?}");
}

#[test]
fn cancellation_reaches_the_engine_in_the_host() {
    let mut c = config();
    // One worker: if the slow render kept running, the next request would
    // wait for it (30 s).
    c.host_workers = 1;
    let engine = engine(c);
    let doc = open(&engine, &Spec::new(3).page(1, Behavior::Slow));
    let pid = stats(&engine).pid;
    let request = full_request(&doc, 1, 0.5, Rotation::R0);
    let cancel = CancelToken::new();
    let canceller = {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            cancel.cancel();
        })
    };
    let started = Instant::now();
    let result = render_with(&doc, &request, PixelFormat::default(), &cancel);
    let cancelled_after = started.elapsed();
    assert_eq!(result.err(), Some(EngineError::Cancelled));
    assert!(
        cancelled_after < Duration::from_secs(2),
        "{cancelled_after:?}"
    );
    canceller
        .join()
        .unwrap_or_else(|_| panic!("canceller panicked"));
    let started = Instant::now();
    assert!(render(&doc, 0).is_ok());
    let next = started.elapsed();
    assert!(next < Duration::from_secs(5), "worker still busy: {next:?}");
    assert_eq!(stats(&engine).pid, pid);
    eprintln!("cancel: returned after {cancelled_after:?}, next render {next:?}");
}

#[test]
fn second_crash_on_a_page_is_permanent() {
    let engine = engine(patient(config()));
    let doc = open(&engine, &Spec::new(4).page(3, Behavior::Abort));
    assert!(render(&doc, 3).is_err());
    let err = render(&doc, 3).expect_err("crashes again");
    assert!(
        matches!(&err, EngineError::Internal(m) if m.contains("will not be retried")),
        "{err}"
    );
    let s = stats(&engine);
    assert_eq!(s.crashes, 2);
    assert_eq!(s.failed_pages, vec![PageIndex::new(3)]);
    // From now on the page fails without touching a host.
    let started = Instant::now();
    let err = render(&doc, 3).expect_err("permanent failure");
    assert!(started.elapsed() < Duration::from_millis(500));
    assert!(
        matches!(&err, EngineError::Internal(m) if m.contains("no longer rendered")),
        "{err}"
    );
    assert_eq!(stats(&engine).crashes, 2);
    assert!(render(&doc, 0).is_ok());
}

#[test]
fn a_crash_storm_stops_restarts() {
    let engine = engine(config());
    let spec = Spec::new(6)
        .page(1, Behavior::Abort)
        .page(2, Behavior::Abort)
        .page(3, Behavior::Abort);
    let doc = open(&engine, &spec);
    assert!(doc.page_info(PageIndex::FIRST).is_ok());
    for page in 1..=3 {
        assert!(render(&doc, page).is_err());
    }
    let s = stats(&engine);
    assert!(s.disabled && s.crashes == 3 && s.pid.is_none(), "{s:?}");
    let started = Instant::now();
    let err = render(&doc, 0).expect_err("restarts are off");
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(
        matches!(&err, EngineError::Internal(m) if m.contains("rendering stopped")),
        "{err}"
    );
}

#[test]
fn a_crash_among_concurrent_requests_is_pinned_on_the_right_page() {
    let mut c = patient(config());
    c.host_workers = 8;
    let mut spec = Spec::new(6);
    for page in [0, 1, 3, 4, 5] {
        spec = spec.page(page, Behavior::Delay);
    }
    spec = spec.page(2, Behavior::Abort);
    let engine = engine(c);
    let doc = Arc::new(open(&engine, &spec));
    let reference = open(&SyntheticEngine, &Spec::new(6));
    for page in 0..6 {
        assert!(doc.page_info(PageIndex::new(page)).is_ok());
    }
    let barrier = Arc::new(Barrier::new(6));
    let workers: Vec<_> = (0..6)
        .map(|page| {
            let doc = Arc::clone(&doc);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                if page == 2 {
                    // Let the slow pages get into the host first.
                    std::thread::sleep(Duration::from_millis(100));
                }
                render(&doc, page)
            })
        })
        .collect();
    let results: Vec<_> = workers
        .into_iter()
        .map(|w| {
            w.join()
                .unwrap_or_else(|_| panic!("render thread panicked"))
        })
        .collect();
    for (page, result) in results.iter().enumerate() {
        if page == 2 {
            assert!(result.is_err(), "page 2 rendered?");
        } else {
            assert_eq!(result, &render(&reference, page as u32), "page {page}");
        }
    }
    let s = stats(&engine);
    // One crash with six requests in flight (nobody to blame yet), one
    // more when page 2 ran alone.
    assert_eq!(s.crashes, 2, "{s:?}");
    assert_eq!(s.failed_pages, vec![PageIndex::new(2)]);
}

#[test]
fn memory_usage_and_trim_reach_the_engine() {
    let engine = engine(config());
    let doc = open(&engine, &Spec::new(3));
    assert!(render(&doc, 0).is_ok() && render(&doc, 1).is_ok());
    let grown = poll("memory usage after renders", || {
        doc.memory_usage().filter(|&b| b > 1 << 20)
    });
    assert!(grown > 1 << 20);
    doc.trim_memory(MemoryPressure::Hard);
    poll("memory usage after a hard trim", || {
        doc.memory_usage().filter(|&b| b == 0)
    });
}

#[test]
fn open_errors_cross_the_process_boundary() {
    let engine = engine(config());
    let spec = Spec::new(2).password("s3cret");
    assert_eq!(
        open_with(&engine, &spec, None).err(),
        Some(EngineError::PasswordRequired)
    );
    assert_eq!(
        open_with(&engine, &spec, Some("nope")).err(),
        Some(EngineError::InvalidPassword)
    );
    let doc = open_with(&engine, &spec, Some("s3cret")).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(doc.metadata().map(|m| m.encrypted), Ok(true));
    for bytes in [b"%PDF-1.7 garbage".to_vec(), Vec::new()] {
        let source = DocumentSource::from_bytes(SharedBytes::from_vec(bytes));
        let result = open_guarded(&engine, source, &OpenOptions::default());
        assert!(
            matches!(result, Err(EngineError::Malformed(_))),
            "{result:?}"
        );
    }
}

#[test]
fn a_crash_while_opening_is_reported_and_the_engine_survives() {
    let engine = engine(config());
    let err = open_with(&engine, &Spec::new(2).on_open(Behavior::Abort), None)
        .err()
        .unwrap_or_else(|| panic!("open succeeded"));
    assert!(
        matches!(&err, EngineError::Internal(m) if m.contains("while opening")),
        "{err}"
    );
    let doc = open(&engine, &Spec::new(2));
    assert!(render(&doc, 0).is_ok());
}

#[test]
fn bad_configurations_fail_cleanly() {
    let mut c = config();
    c.engine = "no-such-engine".into();
    assert!(matches!(
        RemoteEngine::new(c),
        Err(EngineError::Unsupported(_))
    ));
    let mut c = config();
    c.program = r"C:\definitely\missing\fastpdf-host.exe".into();
    assert!(matches!(
        RemoteEngine::new(c),
        Err(EngineError::Internal(m)) if m.contains("cannot start")
    ));
}

fn process_exists(pid: u32) -> bool {
    let out = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .unwrap_or_else(|e| panic!("tasklist: {e}"));
    String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
}

/// Waits for process `pid` to disappear; panics after `limit`.
fn wait_gone(pid: u32, limit: Duration) -> Duration {
    let started = Instant::now();
    while process_exists(pid) {
        assert!(started.elapsed() < limit, "host {pid} is still running");
        std::thread::sleep(Duration::from_millis(20));
    }
    started.elapsed()
}

/// Starts the test host in parent mode and returns it with its host's pid.
fn parent_with_host(mode: &str) -> (std::process::Child, u32) {
    let mut parent = Command::new(HOST)
        .args(["--parent", mode])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn parent: {e}"));
    let stdout = parent.stdout.take().unwrap_or_else(|| panic!("no stdout"));
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .unwrap_or_else(|e| panic!("read: {e}"));
    let pid: u32 = line
        .trim()
        .strip_prefix("host-pid ")
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("unexpected parent output {line:?}"));
    (parent, pid)
}

#[test]
fn the_host_dies_when_its_parent_is_killed() {
    let (mut parent, pid) = parent_with_host("wait");
    assert!(process_exists(pid), "host {pid} is not running");
    parent.kill().unwrap_or_else(|e| panic!("kill parent: {e}"));
    let _ = parent.wait();
    let gone = wait_gone(pid, Duration::from_secs(5));
    eprintln!("parent killed: host gone within {gone:?}");
}

#[test]
fn the_host_dies_when_its_parent_exits_without_cleanup() {
    let (mut parent, pid) = parent_with_host("exit");
    let status = parent.wait().unwrap_or_else(|e| panic!("wait parent: {e}"));
    assert!(status.success());
    let gone = wait_gone(pid, Duration::from_secs(5));
    eprintln!("parent exited: host gone within {gone:?}");
}

#[test]
fn dropping_documents_and_engines_ends_their_hosts() {
    let engine = engine(config());
    let doc = open(&engine, &Spec::new(2));
    let doc_host = stats(&engine)
        .pid
        .unwrap_or_else(|| panic!("no document host"));
    let spare = poll("a spare host", || engine.spare_pid());
    assert_ne!(doc_host, spare);
    assert!(process_exists(doc_host) && process_exists(spare));
    drop(doc);
    let gone = wait_gone(doc_host, Duration::from_secs(5));
    assert!(process_exists(spare), "the spare must survive the document");
    drop(engine);
    let spare_gone = wait_gone(spare, Duration::from_secs(5));
    eprintln!("drop: document host gone within {gone:?}, spare within {spare_gone:?}");
}

/// A marker file for `Behavior::AbortThenStubborn`, unique to this test.
fn marker(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("{name}-{}.marker", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

/// `page_info` on the caller's thread, timed.
fn timed_page_info(
    doc: &GuardedDocument,
    page: u32,
) -> (Result<fastpdf_engine_api::PageInfo, EngineError>, Duration) {
    let started = Instant::now();
    let info = doc.page_info(PageIndex::new(page));
    (info, started.elapsed())
}

#[test]
fn page_geometry_is_served_from_the_cache_without_a_host() {
    let pages = 3000;
    let engine = engine(patient(config()));
    let doc = open(&engine, &Spec::new(pages).page(1, Behavior::Abort));
    let reference = open(&SyntheticEngine, &Spec::new(pages));
    poll("all page geometry", || {
        (stats(&engine).geometry_missing == 0).then_some(())
    });
    assert!(render(&doc, 1).is_err());
    assert_eq!(stats(&engine).pid, None, "the host should be gone");
    let started = Instant::now();
    for page in 0..pages {
        let p = PageIndex::new(page);
        assert_eq!(doc.page_info(p), reference.page_info(p), "page {page}");
    }
    let took = started.elapsed();
    assert!(
        took < Duration::from_millis(500),
        "{pages} page infos took {took:?}"
    );
    // Nothing was restarted for that.
    let s = stats(&engine);
    assert_eq!((s.pid, s.restarts), (None, 0), "{s:?}");
    eprintln!("page_info x{pages} without a host: {took:?}");
}

#[test]
fn page_geometry_skips_busy_render_workers_and_the_gate() {
    let pages = 3000;
    let mut c = patient(config());
    c.host_workers = 1;
    c.request_timeout = Some(Duration::from_secs(2));
    // The background batch that contains the last page never finishes in
    // the host, so pages from 2112 on stay unknown; page 2 renders slowly.
    let spec = Spec::new(pages)
        .geometry_hang(pages - 1)
        .page(2, Behavior::Slow);
    let engine = engine(c);
    let doc = Arc::new(open(&engine, &spec));
    let reference = open(&SyntheticEngine, &Spec::new(pages));
    poll("the first background batches", || {
        (stats(&engine).geometry_missing <= pages - 2112).then_some(())
    });

    // The host's only render worker is busy with page 2.
    let cancel = CancelToken::new();
    let busy = {
        let doc = Arc::clone(&doc);
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            let request = full_request(&doc, 2, 0.5, Rotation::R0);
            render_with(&doc, &request, PixelFormat::default(), &cancel).map(drop)
        })
    };
    std::thread::sleep(Duration::from_millis(150));
    let (info, took) = timed_page_info(&doc, 2500);
    assert_eq!(info, reference.page_info(PageIndex::new(2500)));
    assert!(
        took < Duration::from_millis(300),
        "waited for the render worker: {took:?}"
    );
    let first = took;

    // A page whose geometry never comes: bounded wait, then an error, and
    // the next unknown page is not held up behind it.
    let (info, took) = timed_page_info(&doc, pages - 1);
    assert!(info.is_err());
    assert!(
        took >= Duration::from_millis(900) && took < Duration::from_millis(1500),
        "worst case: {took:?}"
    );
    let worst = took;
    let (info, took) = timed_page_info(&doc, 2700);
    assert_eq!(info, reference.page_info(PageIndex::new(2700)));
    assert!(
        took < Duration::from_millis(300),
        "held up by a hung page: {took:?}"
    );

    // The stuck batch overruns its deadline; on the new host it is retried
    // exclusively and hangs again, holding (or queued for) the gate.
    poll("a replacement host", || {
        let s = stats(&engine);
        (s.crashes >= 1 && s.pid.is_some()).then_some(())
    });
    std::thread::sleep(Duration::from_millis(300));
    let (info, took) = timed_page_info(&doc, 2600);
    assert_eq!(info, reference.page_info(PageIndex::new(2600)));
    assert!(
        took < Duration::from_millis(300),
        "waited for the gate: {took:?}"
    );
    eprintln!(
        "unknown page_info: busy worker {first:?}, exclusive gate {took:?}, hung page (bound) {worst:?}"
    );
    cancel.cancel();
    let _ = busy.join();
}

#[test]
fn a_cancelled_exclusive_render_keeps_admission_until_the_host_is_done() {
    let marker = marker("cancelled-exclusive");
    let engine = engine(patient(config()));
    let spec = Spec::new(4)
        .page(1, Behavior::AbortThenStubborn)
        .marker(&marker);
    let doc = open(&engine, &spec);
    let reference = open(&SyntheticEngine, &Spec::new(4));
    assert!(render(&doc, 1).is_err(), "the first render aborts the host");

    // Page 1 is suspect now: it runs exclusively, and the host spends 1.5 s
    // on it whatever we say.
    let cancel = CancelToken::new();
    let canceller = {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            cancel.cancel();
        })
    };
    let request = full_request(&doc, 1, 0.5, Rotation::R0);
    let started = Instant::now();
    let result = render_with(&doc, &request, PixelFormat::default(), &cancel);
    let returned = started.elapsed();
    assert_eq!(result.err(), Some(EngineError::Cancelled));
    assert!(
        returned < Duration::from_millis(800),
        "the caller waited: {returned:?}"
    );
    canceller
        .join()
        .unwrap_or_else(|_| panic!("canceller panicked"));

    // Nothing else may enter before the host has finished page 1.
    let next = render(&doc, 0);
    let admitted = started.elapsed();
    assert_eq!(next, render(&reference, 0));
    assert!(
        admitted >= Duration::from_millis(1300),
        "admitted while the cancelled exclusive render still ran: {admitted:?}"
    );
    let _ = std::fs::remove_file(&marker);
    eprintln!(
        "cancelled exclusive: caller back after {returned:?}, next admitted after {admitted:?}"
    );
}
