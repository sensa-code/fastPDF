//! `corpus`: run `full` on every file of a manifest or directory, each in its
//! own child process (per-file peak RSS, crash and hang isolation).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::args::Args;
use crate::engines;
use crate::metrics;
use crate::report::{CorpusReport, FileReport, SCHEMA, Stats, Status};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Entry {
    pub(crate) path: PathBuf,
    /// Stable identifier: path relative to the corpus root, `/`-separated.
    pub(crate) id: String,
    pub(crate) password: Option<String>,
    pub(crate) category: Option<String>,
    pub(crate) expect: Option<String>,
}

pub(crate) fn run(input: &Path, args: &Args) -> Result<ExitCode, String> {
    let engine = engines::select(args.engine.as_deref())?;
    let engine_name = engine.info().name.to_owned();
    let entries = discover(input)?;
    if entries.is_empty() {
        return Err(format!("no PDF files found in {}", input.display()));
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut results = Vec::with_capacity(entries.len());
    for (i, entry) in entries.iter().enumerate() {
        let runs: Vec<FileReport> = (0..args.repeat)
            .map(|_| run_child(&exe, entry, &engine_name, args))
            .collect();
        let report = merge(runs);
        eprintln!(
            "[{:>3}/{}] {:<10} {:<60} open {:>8} first {:>9} rss {:>7}",
            i + 1,
            entries.len(),
            format!("{:?}", report.status).to_lowercase(),
            entry.id,
            fmt_opt(report.open_ms, "ms"),
            fmt_opt(report.first_page_ms, "ms"),
            fmt_opt(report.rss_peak_mb, "MB"),
        );
        results.push(report);
    }

    let robustness_failures = results
        .iter()
        .filter(|r| matches!(r.status, Status::Crash | Status::Timeout))
        .count();
    let report = CorpusReport {
        schema: SCHEMA,
        created_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        machine: metrics::machine(),
        rustc: env!("FASTPDF_RUSTC_VERSION").to_owned(),
        profile: env!("FASTPDF_BUILD_PROFILE").to_owned(),
        git_rev: git_rev(),
        engine: engine_name,
        display_scale: args.scale,
        tile_size: args.tile,
        workers: args.workers,
        runs_per_file: args.repeat,
        results,
    };
    let text = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    match &args.out {
        Some(path) => {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))?;
            eprintln!("wrote {}", path.display());
        }
        None => println!("{text}"),
    }
    if robustness_failures > 0 {
        eprintln!(
            "{robustness_failures} file(s) crashed or timed out — the reader must never do that"
        );
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

fn fmt_opt(v: Option<f64>, unit: &str) -> String {
    v.map_or_else(|| "-".into(), |v| format!("{v:.1}{unit}"))
}

/// Entries from a JSON manifest (an array, or an object holding one under
/// `files`/`entries`/`fixtures`) or every `*.pdf` below a directory.
pub(crate) fn discover(input: &Path) -> Result<Vec<Entry>, String> {
    if input.is_dir() {
        let mut paths = Vec::new();
        walk(input, &mut paths).map_err(|e| format!("{}: {e}", input.display()))?;
        paths.sort();
        return Ok(paths
            .into_iter()
            .map(|path| Entry {
                id: relative_id(input, &path),
                path,
                password: None,
                category: None,
                expect: None,
            })
            .collect());
    }
    let text = std::fs::read_to_string(input).map_err(|e| format!("{}: {e}", input.display()))?;
    let json: Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", input.display()))?;
    let root = input.parent().unwrap_or(Path::new("."));
    let list = match &json {
        Value::Array(items) => items.as_slice(),
        Value::Object(map) => ["files", "entries", "fixtures"]
            .iter()
            .find_map(|k| map.get(*k).and_then(Value::as_array))
            .map(Vec::as_slice)
            .ok_or("manifest has no files/entries/fixtures array")?,
        _ => return Err("manifest must be a JSON array or object".into()),
    };
    let str_field = |item: &Value, keys: &[&str]| -> Option<String> {
        keys.iter()
            .find_map(|k| item.get(*k).and_then(Value::as_str))
            .map(str::to_owned)
    };
    let mut entries = Vec::new();
    for item in list {
        let Some(rel) = str_field(item, &["path", "file", "relative_path"]) else {
            continue;
        };
        let path = root.join(&rel);
        entries.push(Entry {
            id: relative_id(root, &path),
            path,
            password: str_field(item, &["password", "user_password"]),
            category: str_field(item, &["category"]),
            expect: str_field(item, &["expect", "expected"]),
        });
    }
    Ok(entries)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            walk(&path, out)?;
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
        {
            out.push(path);
        }
    }
    Ok(())
}

fn relative_id(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn run_child(exe: &Path, entry: &Entry, engine: &str, args: &Args) -> FileReport {
    let mut cmd = Command::new(exe);
    cmd.arg("full")
        .arg(&entry.path)
        .args(["--engine", engine])
        .args(["--scale", &args.scale.to_string()])
        .args(["--samples", &args.samples.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(tile) = args.tile {
        cmd.args(["--tile", &tile.to_string()]);
    }
    if let Some(workers) = args.workers {
        cmd.args(["--workers", &workers.to_string()]);
    }
    if let Some(pw) = entry.password.as_ref().or(args.password.as_ref()) {
        cmd.args(["--password", pw]);
    }

    let mut report = match spawn_with_timeout(cmd, Duration::from_secs(args.timeout_secs)) {
        Ok(out) => match serde_json::from_slice::<FileReport>(&out.stdout) {
            Ok(r) => r,
            Err(_) => failure(entry, engine, args, Status::Crash, &out),
        },
        Err(out) => failure(entry, engine, args, Status::Timeout, &out),
    };
    report.file = entry.id.clone();
    report.category = entry.category.clone();
    report.expect = entry.expect.clone();
    report
}

pub(crate) struct ChildOutput {
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) status: Option<std::process::ExitStatus>,
}

pub(crate) fn spawn_with_timeout(
    mut cmd: Command,
    timeout: Duration,
) -> Result<ChildOutput, ChildOutput> {
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Err(ChildOutput {
                stdout: Vec::new(),
                stderr: e.to_string().into_bytes(),
                status: None,
            });
        }
    };
    // Drain pipes on threads so a chatty child cannot block on a full pipe.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let output = ChildOutput {
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
        status,
    };
    if status.is_some() {
        Ok(output)
    } else {
        Err(output)
    }
}

fn failure(
    entry: &Entry,
    engine: &str,
    args: &Args,
    status: Status,
    out: &ChildOutput,
) -> FileReport {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let tail: String = stderr
        .chars()
        .rev()
        .take(2000)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let code = out.status.and_then(|s| s.code());
    let mut report = empty_report(entry, engine, args);
    report.status = status;
    report.error = Some(match (status, code) {
        (Status::Timeout, _) => format!("timed out after {} s", args.timeout_secs),
        (_, Some(code)) => format!("child exited with code {code:#x} without a report"),
        _ => "child terminated without a report".into(),
    });
    report.stderr = (!tail.is_empty()).then_some(tail);
    report
}

fn empty_report(entry: &Entry, engine: &str, args: &Args) -> FileReport {
    FileReport {
        schema: SCHEMA,
        mode: "full".into(),
        file: entry.id.clone(),
        category: entry.category.clone(),
        expect: entry.expect.clone(),
        file_bytes: std::fs::metadata(&entry.path).ok().map(|m| m.len()),
        engine: engine.into(),
        engine_version: String::new(),
        status: Status::Crash,
        error: None,
        page_count: None,
        load_strategy: None,
        display_scale: args.scale,
        tile_size: args.tile,
        workers: args.workers,
        repeat: 1,
        read_ms: None,
        open_ms: None,
        metadata_ms: None,
        first_page_ms: None,
        time_to_first_page_ms: None,
        samples_ms: None,
        page_render_ms: None,
        text_ms: None,
        text_chars: None,
        thumbnail_ms: None,
        tiles: None,
        rss_peak_mb: None,
        private_peak_mb: None,
        cpu_ms: None,
        page_errors: Vec::new(),
        stderr: None,
    }
}

/// Combines repeated child runs: robustness failures win; otherwise every
/// scalar becomes the median across runs.
fn merge(mut runs: Vec<FileReport>) -> FileReport {
    if let Some(bad) = runs
        .iter()
        .position(|r| matches!(r.status, Status::Crash | Status::Timeout))
    {
        return runs.swap_remove(bad);
    }
    let n = runs.len() as u32;
    let median = |f: &dyn Fn(&FileReport) -> Option<f64>| -> Option<f64> {
        let v: Vec<f64> = runs.iter().filter_map(f).collect();
        Stats::from_samples(&v).map(|s| s.median)
    };
    let median_stats = |f: &dyn Fn(&FileReport) -> Option<&Stats>| -> Option<Stats> {
        let mut v: Vec<&Stats> = runs.iter().filter_map(f).collect();
        v.sort_by(|a, b| a.median.total_cmp(&b.median));
        v.get(v.len() / 2).map(|s| (*s).clone())
    };
    let mut merged = runs[0].clone();
    merged.repeat = n;
    merged.read_ms = median(&|r| r.read_ms);
    merged.open_ms = median(&|r| r.open_ms);
    merged.metadata_ms = median(&|r| r.metadata_ms);
    merged.first_page_ms = median(&|r| r.first_page_ms);
    merged.time_to_first_page_ms = median(&|r| r.time_to_first_page_ms);
    merged.text_ms = median(&|r| r.text_ms);
    merged.rss_peak_mb = median(&|r| r.rss_peak_mb);
    merged.private_peak_mb = median(&|r| r.private_peak_mb);
    merged.cpu_ms = median(&|r| r.cpu_ms);
    merged.page_render_ms = median_stats(&|r| r.page_render_ms.as_ref());
    merged.thumbnail_ms = median_stats(&|r| r.thumbnail_ms.as_ref());
    merged
}

fn git_rev() -> Option<String> {
    let rev = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let mut rev = String::from_utf8_lossy(&rev.stdout).trim().to_owned();
    let dirty = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .ok()
        .is_some_and(|o| !o.stdout.is_empty());
    if dirty {
        rev.push_str("-dirty");
    }
    Some(rev)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_entries_resolve_relative_to_the_manifest() {
        let dir = std::env::temp_dir().join(format!("fastpdf-bench-corpus-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = dir.join("manifest.json");
        std::fs::write(
            &manifest,
            r#"{"files":[{"path":"a/b.pdf","category":"small-text","password":"pw","expect":"open_ok"},{"note":"no path"}]}"#,
        )
        .unwrap();
        let entries = discover(&manifest).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "a/b.pdf");
        assert_eq!(entries[0].path, dir.join("a/b.pdf"));
        assert_eq!(entries[0].password.as_deref(), Some("pw"));
        assert_eq!(entries[0].category.as_deref(), Some("small-text"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn robustness_failures_win_merges() {
        let args = crate::args::parse(["full", "x.pdf"].map(Into::into)).unwrap();
        let entry = Entry {
            path: "x.pdf".into(),
            id: "x.pdf".into(),
            password: None,
            category: None,
            expect: None,
        };
        let mut ok = empty_report(&entry, "e", &args);
        ok.status = Status::Ok;
        ok.open_ms = Some(1.0);
        let mut ok2 = ok.clone();
        ok2.open_ms = Some(3.0);
        let mut ok3 = ok.clone();
        ok3.open_ms = Some(2.0);
        assert_eq!(merge(vec![ok.clone(), ok2, ok3]).open_ms, Some(2.0));
        let mut crash = ok.clone();
        crash.status = Status::Crash;
        assert_eq!(merge(vec![ok, crash]).status, Status::Crash);
    }
}
