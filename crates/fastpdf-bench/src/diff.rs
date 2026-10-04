//! `diff` / `diff-corpus`: render the same pages with two engines and
//! measure how far apart the results are (M4 correctness comparison,
//! spec §44: let the data decide).

use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use fastpdf_core::loader;
use fastpdf_engine_api::{
    DocumentSource, EngineDocument, GuardedDocument, OpenOptions, PageIndex, PdfEngine,
    PixelFormat, Pixmap, ResourceLimits, open_guarded,
};
use fastpdf_render::ScaleBucket;
use serde::{Deserialize, Serialize};

use crate::args::Args;
use crate::corpus::{self, spawn_with_timeout};
use crate::engines;
use crate::measure::render_full;
use crate::png;
use crate::report::{SCHEMA, ms};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct EngineOpen {
    pub(crate) engine: String,
    pub(crate) ok: bool,
    pub(crate) open_ms: Option<f64>,
    pub(crate) page_count: Option<u32>,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct PageDiff {
    /// 1-based page number.
    pub(crate) page: u32,
    pub(crate) size_a: Option<[u32; 2]>,
    pub(crate) size_b: Option<[u32; 2]>,
    pub(crate) render_ms_a: Option<f64>,
    pub(crate) render_ms_b: Option<f64>,
    pub(crate) error_a: Option<String>,
    pub(crate) error_b: Option<String>,
    /// Share of pixels whose largest channel difference exceeds the tolerance.
    pub(crate) differing_pct: Option<f64>,
    pub(crate) max_delta: Option<u8>,
    pub(crate) mean_abs_delta: Option<f64>,
    /// `None` when the images are identical (infinite PSNR) or not comparable.
    pub(crate) psnr_db: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct DiffReport {
    pub(crate) schema: u32,
    pub(crate) mode: String,
    pub(crate) file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) expect: Option<String>,
    pub(crate) display_scale: f32,
    pub(crate) tolerance: u8,
    pub(crate) a: EngineOpen,
    pub(crate) b: EngineOpen,
    pub(crate) pages: Vec<PageDiff>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) failure: Option<String>,
}

/// `diff <file>`: compares page 1, the middle page and the last page (or
/// `--page N` only) between two engines.
pub(crate) fn run(file: &Path, args: &Args) -> Result<ExitCode, String> {
    let (a, b) = engines::select_pair(args.engine.as_deref())?;
    let report = diff_file(a.as_ref(), b.as_ref(), file, args);
    let text = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    println!("{text}");
    Ok(ExitCode::SUCCESS)
}

fn open(engine: &dyn PdfEngine, file: &Path, args: &Args) -> (EngineOpen, Option<GuardedDocument>) {
    let name = engine.info().name.to_owned();
    let started = Instant::now();
    let result = loader::load(file)
        .map_err(|e| e.to_string())
        .and_then(|loaded| {
            let options = OpenOptions {
                password: args.password.clone(),
                ..OpenOptions::default()
            };
            let source = DocumentSource::from_bytes(loaded.bytes).with_path(file);
            open_guarded(engine, source, &options).map_err(|e| e.to_string())
        });
    let open_ms = Some(ms(started.elapsed()));
    match result {
        Ok(doc) => (
            EngineOpen {
                engine: name,
                ok: true,
                open_ms,
                page_count: Some(doc.page_count()),
                error: None,
            },
            Some(doc),
        ),
        Err(error) => (
            EngineOpen {
                engine: name,
                ok: false,
                open_ms,
                page_count: None,
                error: Some(error),
            },
            None,
        ),
    }
}

fn diff_file(a: &dyn PdfEngine, b: &dyn PdfEngine, file: &Path, args: &Args) -> DiffReport {
    let (open_a, doc_a) = open(a, file, args);
    let (open_b, doc_b) = open(b, file, args);
    let mut report = DiffReport {
        schema: SCHEMA,
        mode: "diff".into(),
        file: file.to_string_lossy().replace('\\', "/"),
        category: None,
        expect: None,
        display_scale: args.scale,
        tolerance: args.tolerance,
        a: open_a,
        b: open_b,
        pages: Vec::new(),
        failure: None,
    };
    let (Some(doc_a), Some(doc_b)) = (doc_a, doc_b) else {
        return report;
    };
    let count = doc_a.page_count().min(doc_b.page_count());
    let pages: Vec<u32> = if args.page_given {
        vec![args.page - 1]
    } else {
        let mut p = vec![0, count / 2, count.saturating_sub(1)];
        p.dedup();
        p
    };
    let scale = ScaleBucket::for_display_scale(args.scale).render_scale();
    for index in pages.into_iter().filter(|p| *p < count) {
        let page = PageIndex::new(index);
        let (ra, ta) = timed(|| render_full(&doc_a, page, scale));
        let (rb, tb) = timed(|| render_full(&doc_b, page, scale));
        let mut d = PageDiff {
            page: page.display_number(),
            size_a: ra.as_ref().ok().map(|p| [p.size().width, p.size().height]),
            size_b: rb.as_ref().ok().map(|p| [p.size().width, p.size().height]),
            render_ms_a: ra.is_ok().then(|| ms(ta)),
            render_ms_b: rb.is_ok().then(|| ms(tb)),
            error_a: ra.as_ref().err().map(ToString::to_string),
            error_b: rb.as_ref().err().map(ToString::to_string),
            differing_pct: None,
            max_delta: None,
            mean_abs_delta: None,
            psnr_db: None,
        };
        if let (Ok(pa), Ok(pb)) = (&ra, &rb)
            && pa.size() == pb.size()
        {
            let stats = compare(pa, pb, args.tolerance);
            d.differing_pct = Some(stats.differing_pct);
            d.max_delta = Some(stats.max_delta);
            d.mean_abs_delta = Some(stats.mean_abs_delta);
            d.psnr_db = stats.psnr_db;
            if let Some(dir) = &args.out {
                write_images(dir, file, page, &report, pa, pb, args.tolerance);
            }
        }
        report.pages.push(d);
    }
    report
}

fn timed<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let started = Instant::now();
    let value = f();
    (value, started.elapsed())
}

struct Stats {
    differing_pct: f64,
    max_delta: u8,
    mean_abs_delta: f64,
    psnr_db: Option<f64>,
}

/// Both pixmaps are rendered on an opaque white background, so comparing
/// premultiplied RGB is the same as comparing visible colors.
fn compare(a: &Pixmap, b: &Pixmap, tolerance: u8) -> Stats {
    let (pa, _) = a.data().as_chunks::<4>();
    let (pb, _) = b.data().as_chunks::<4>();
    let swap_b = a.format() != b.format();
    let mut differing = 0u64;
    let mut max_delta = 0u8;
    let mut sum_abs = 0u64;
    let mut sum_sq = 0u64;
    for (x, y) in pa.iter().zip(pb) {
        let y = if swap_b { [y[2], y[1], y[0], y[3]] } else { *y };
        let mut px_max = 0u8;
        for c in 0..3 {
            let d = x[c].abs_diff(y[c]);
            px_max = px_max.max(d);
            sum_abs += u64::from(d);
            sum_sq += u64::from(d) * u64::from(d);
        }
        max_delta = max_delta.max(px_max);
        if px_max > tolerance {
            differing += 1;
        }
    }
    let pixels = pa.len().max(1) as f64;
    let samples = pixels * 3.0;
    let mse = sum_sq as f64 / samples;
    Stats {
        differing_pct: (differing as f64 / pixels * 100.0 * 1000.0).round() / 1000.0,
        max_delta,
        mean_abs_delta: (sum_abs as f64 / samples * 1000.0).round() / 1000.0,
        psnr_db: (mse > 0.0)
            .then(|| ((10.0 * (255.0f64 * 255.0 / mse).log10()) * 100.0).round() / 100.0),
    }
}

fn write_images(
    dir: &Path,
    file: &Path,
    page: PageIndex,
    report: &DiffReport,
    a: &Pixmap,
    b: &Pixmap,
    tolerance: u8,
) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let stem = file
        .file_stem()
        .map_or_else(|| "page".into(), |s| s.to_string_lossy().into_owned());
    let base = format!("{stem}-p{}", page.display_number());
    let _ = png::write_rgba(&dir.join(format!("{base}-{}.png", report.a.engine)), a);
    let _ = png::write_rgba(&dir.join(format!("{base}-{}.png", report.b.engine)), b);
    if let Ok(diff) = diff_image(a, b, tolerance) {
        let _ = png::write_rgba(&dir.join(format!("{base}-diff.png")), &diff);
    }
}

/// Faded copy of `a` with differing pixels painted red.
fn diff_image(
    a: &Pixmap,
    b: &Pixmap,
    tolerance: u8,
) -> Result<Pixmap, fastpdf_engine_api::EngineError> {
    let mut out = Pixmap::new(
        a.size(),
        PixelFormat::Rgba8Premultiplied,
        &ResourceLimits::default(),
    )?;
    let swap_a = a.format() == PixelFormat::Bgra8Premultiplied;
    let swap_b = b.format() == PixelFormat::Bgra8Premultiplied;
    let (pa, _) = a.data().as_chunks::<4>();
    let (pb, _) = b.data().as_chunks::<4>();
    let mut view = out.as_mut();
    let (dst, _) = view.data_mut().as_chunks_mut::<4>();
    for ((d, x), y) in dst.iter_mut().zip(pa).zip(pb) {
        let x = if swap_a { [x[2], x[1], x[0], x[3]] } else { *x };
        let y = if swap_b { [y[2], y[1], y[0], y[3]] } else { *y };
        let delta = (0..3).map(|c| x[c].abs_diff(y[c])).max().unwrap_or(0);
        *d = if delta > tolerance {
            [255, 0, 0, 255]
        } else {
            let fade = |v: u8| 192 + v / 4;
            [fade(x[0]), fade(x[1]), fade(x[2]), 255]
        };
    }
    Ok(out)
}

/// `diff-corpus <manifest|dir>`: `diff` for every file, each in a child
/// process; prints a Markdown summary and optionally writes JSON.
pub(crate) fn run_corpus(input: &Path, args: &Args) -> Result<ExitCode, String> {
    let (a, b) = engines::select_pair(args.engine.as_deref())?;
    let names = format!("{},{}", a.info().name, b.info().name);
    let entries = corpus::discover(input)?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut reports = Vec::with_capacity(entries.len());
    for (i, entry) in entries.iter().enumerate() {
        let mut cmd = Command::new(&exe);
        cmd.arg("diff")
            .arg(&entry.path)
            .args(["--engine", &names])
            .args(["--scale", &args.scale.to_string()])
            .args(["--tolerance", &args.tolerance.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(pw) = entry.password.as_ref().or(args.password.as_ref()) {
            cmd.args(["--password", pw]);
        }
        if let Some(dir) = &args.out_images {
            cmd.arg("--out").arg(dir);
        }
        let mut report = match spawn_with_timeout(cmd, Duration::from_secs(args.timeout_secs)) {
            Ok(out) => serde_json::from_slice::<DiffReport>(&out.stdout)
                .unwrap_or_else(|_| failed(&entry.id, &names, args, "crash (no report)")),
            Err(_) => failed(&entry.id, &names, args, "timeout"),
        };
        report.file = entry.id.clone();
        report.category = entry.category.clone();
        report.expect = entry.expect.clone();
        eprintln!("[{:>3}/{}] {}", i + 1, entries.len(), summary_line(&report));
        reports.push(report);
    }
    println!("{}", markdown(&reports));
    if let Some(path) = &args.out {
        let text = serde_json::to_string_pretty(&reports).map_err(|e| e.to_string())?;
        std::fs::write(path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))?;
        eprintln!("wrote {}", path.display());
    }
    Ok(ExitCode::SUCCESS)
}

fn failed(file: &str, names: &str, args: &Args, why: &str) -> DiffReport {
    let mut engines = names.split(',');
    let mut side = || EngineOpen {
        engine: engines.next().unwrap_or_default().to_owned(),
        ok: false,
        open_ms: None,
        page_count: None,
        error: None,
    };
    DiffReport {
        schema: SCHEMA,
        mode: "diff".into(),
        file: file.to_owned(),
        category: None,
        expect: None,
        display_scale: args.scale,
        tolerance: args.tolerance,
        a: side(),
        b: side(),
        pages: Vec::new(),
        failure: Some(why.to_owned()),
    }
}

fn summary_line(r: &DiffReport) -> String {
    let worst = r
        .pages
        .iter()
        .filter_map(|p| p.differing_pct)
        .fold(None::<f64>, |m, v| Some(m.map_or(v, |m| m.max(v))));
    format!(
        "{:<56} {}={} {}={} worst diff {}",
        r.file,
        r.a.engine,
        if r.a.ok { "ok" } else { "ERR" },
        r.b.engine,
        if r.b.ok { "ok" } else { "ERR" },
        worst.map_or_else(|| "-".into(), |w| format!("{w:.3}%")),
    )
}

/// Markdown table for docs/engine-comparison.md.
fn markdown(reports: &[DiffReport]) -> String {
    let (a, b) = reports
        .first()
        .map_or(("a", "b"), |r| (r.a.engine.as_str(), r.b.engine.as_str()));
    let mut out = format!(
        "| File | Category | {a} open | {b} open | Pages | Worst differing % | Max Δ | {a} render ms | {b} render ms |\n|---|---|---|---|---|---|---|---|---|\n"
    );
    for r in reports {
        let worst = r
            .pages
            .iter()
            .filter_map(|p| p.differing_pct)
            .fold(0.0f64, f64::max);
        let max_delta = r.pages.iter().filter_map(|p| p.max_delta).max();
        let sum = |f: fn(&PageDiff) -> Option<f64>| -> String {
            let v: Vec<f64> = r.pages.iter().filter_map(f).collect();
            if v.is_empty() {
                "-".into()
            } else {
                format!("{:.1}", v.iter().sum::<f64>())
            }
        };
        let status = |o: &EngineOpen| -> String {
            match (&r.failure, o.ok) {
                (Some(f), _) => f.clone(),
                (None, true) => format!("ok ({:.1} ms)", o.open_ms.unwrap_or_default()),
                (None, false) => format!(
                    "error: {}",
                    o.error
                        .as_deref()
                        .unwrap_or("?")
                        .chars()
                        .take(48)
                        .collect::<String>()
                ),
            }
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            r.file,
            r.category.as_deref().unwrap_or("-"),
            status(&r.a),
            status(&r.b),
            r.pages.len(),
            if r.pages.iter().any(|p| p.differing_pct.is_some()) {
                format!("{worst:.3}")
            } else {
                "-".into()
            },
            max_delta.map_or_else(|| "-".into(), |d| d.to_string()),
            sum(|p| p.render_ms_a),
            sum(|p| p.render_ms_b),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastpdf_engine_api::{PixelSize, Rgba8};

    fn solid(color: Rgba8, format: PixelFormat) -> Pixmap {
        let mut p = Pixmap::new(PixelSize::new(4, 4), format, &ResourceLimits::default()).unwrap();
        p.as_mut().fill(color);
        p
    }

    #[test]
    fn identical_images_have_no_difference() {
        let a = solid(Rgba8::WHITE, PixelFormat::Rgba8Premultiplied);
        let s = compare(&a, &a.clone(), 16);
        assert_eq!((s.differing_pct, s.max_delta, s.psnr_db), (0.0, 0, None));
    }

    #[test]
    fn channel_order_is_normalized() {
        let a = solid(Rgba8::new(255, 0, 0, 255), PixelFormat::Rgba8Premultiplied);
        let b = solid(Rgba8::new(255, 0, 0, 255), PixelFormat::Bgra8Premultiplied);
        assert_eq!(compare(&a, &b, 0).max_delta, 0);
    }

    #[test]
    fn differences_are_measured() {
        let a = solid(Rgba8::WHITE, PixelFormat::Rgba8Premultiplied);
        let b = solid(
            Rgba8::new(235, 255, 255, 255),
            PixelFormat::Rgba8Premultiplied,
        );
        let s = compare(&a, &b, 16);
        assert_eq!((s.differing_pct, s.max_delta), (100.0, 20));
        assert!(s.psnr_db.is_some_and(|p| p > 25.0 && p < 40.0));
        assert_eq!(compare(&a, &b, 20).differing_pct, 0.0);
        let d = diff_image(&a, &b, 16).unwrap();
        assert_eq!(&d.data()[..4], &[255, 0, 0, 255]);
    }
}
