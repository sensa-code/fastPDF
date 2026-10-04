//! `compare`: flag regressions between two corpus runs (spec §30: > 10%).

use std::collections::HashMap;
use std::path::Path;
use std::process::ExitCode;

use crate::args::Args;
use crate::report::{CorpusReport, FileReport, SCHEMA, Status};

/// Differences below these absolute amounts are noise, whatever the percentage.
const NOISE_MS: f64 = 0.5;
const NOISE_MB: f64 = 1.0;

struct Metric {
    name: &'static str,
    unit: &'static str,
    get: fn(&FileReport) -> Option<f64>,
}

const METRICS: &[Metric] = &[
    Metric {
        name: "open_ms",
        unit: "ms",
        get: |r| r.open_ms,
    },
    Metric {
        name: "first_page_ms",
        unit: "ms",
        get: |r| r.first_page_ms,
    },
    Metric {
        name: "time_to_first_page_ms",
        unit: "ms",
        get: |r| r.time_to_first_page_ms,
    },
    Metric {
        name: "page_render_ms.median",
        unit: "ms",
        get: |r| r.page_render_ms.as_ref().map(|s| s.median),
    },
    Metric {
        name: "text_ms",
        unit: "ms",
        get: |r| r.text_ms,
    },
    Metric {
        name: "thumbnail_ms.median",
        unit: "ms",
        get: |r| r.thumbnail_ms.as_ref().map(|s| s.median),
    },
    Metric {
        name: "rss_peak_mb",
        unit: "MB",
        get: |r| r.rss_peak_mb,
    },
    Metric {
        name: "cpu_ms",
        unit: "ms",
        get: |r| r.cpu_ms,
    },
];

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Finding {
    pub(crate) file: String,
    pub(crate) metric: String,
    pub(crate) baseline: String,
    pub(crate) candidate: String,
    pub(crate) delta_percent: Option<f64>,
    pub(crate) regression: bool,
}

pub(crate) fn run(baseline: &Path, candidate: &Path, args: &Args) -> Result<ExitCode, String> {
    let a = load(baseline)?;
    let b = load(candidate)?;
    if a.machine != b.machine {
        eprintln!("warning: runs come from different machines; numbers are not comparable");
    }
    if (a.engine.as_str(), a.display_scale, a.tile_size)
        != (b.engine.as_str(), b.display_scale, b.tile_size)
    {
        eprintln!("warning: engine or render settings differ between runs");
    }
    let findings = compare(&a, &b, args.threshold);
    let regressions = findings.iter().filter(|f| f.regression).count();
    let improvements = findings.len() - regressions;
    println!(
        "{:<52} {:<24} {:>12} {:>12} {:>8}",
        "file", "metric", "baseline", "candidate", "delta"
    );
    for f in &findings {
        println!(
            "{:<52} {:<24} {:>12} {:>12} {:>8} {}",
            truncate(&f.file, 52),
            f.metric,
            f.baseline,
            f.candidate,
            f.delta_percent
                .map_or_else(|| "-".into(), |d| format!("{d:+.1}%")),
            if f.regression {
                "REGRESSION"
            } else {
                "improved"
            },
        );
    }
    println!(
        "\n{regressions} regression(s), {improvements} improvement(s) beyond {}% ({} files compared)",
        args.threshold,
        b.results.len()
    );
    Ok(if regressions > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn load(path: &Path) -> Result<CorpusReport, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let report: CorpusReport =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if report.schema != SCHEMA {
        return Err(format!(
            "{}: schema {} (expected {SCHEMA})",
            path.display(),
            report.schema
        ));
    }
    Ok(report)
}

pub(crate) fn compare(a: &CorpusReport, b: &CorpusReport, threshold: f64) -> Vec<Finding> {
    let base: HashMap<&str, &FileReport> = a.results.iter().map(|r| (r.file.as_str(), r)).collect();
    let mut out = Vec::new();
    for cand in &b.results {
        let Some(old) = base.get(cand.file.as_str()) else {
            continue;
        };
        if old.status != cand.status {
            let worse = rank(cand.status) > rank(old.status);
            out.push(Finding {
                file: cand.file.clone(),
                metric: "status".into(),
                baseline: format!("{:?}", old.status),
                candidate: format!("{:?}", cand.status),
                delta_percent: None,
                regression: worse,
            });
        }
        for m in METRICS {
            let (Some(x), Some(y)) = ((m.get)(old), (m.get)(cand)) else {
                continue;
            };
            let noise = if m.unit == "MB" { NOISE_MB } else { NOISE_MS };
            if x <= 0.0 || (y - x).abs() < noise {
                continue;
            }
            let delta = (y - x) / x * 100.0;
            if delta.abs() > threshold {
                out.push(Finding {
                    file: cand.file.clone(),
                    metric: m.name.into(),
                    baseline: format!("{x:.2}{}", m.unit),
                    candidate: format!("{y:.2}{}", m.unit),
                    delta_percent: Some(delta),
                    regression: delta > 0.0,
                });
            }
        }
    }
    out
}

/// Higher is worse.
fn rank(status: Status) -> u8 {
    match status {
        Status::Ok => 0,
        Status::Partial => 1,
        Status::OpenError | Status::LoadError => 2,
        Status::Timeout => 3,
        Status::Crash => 4,
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let tail: String = s
            .chars()
            .rev()
            .take(max - 1)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("…{tail}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::MachineInfo;

    fn corpus(open_ms: f64, rss: f64, status: Status) -> CorpusReport {
        let args = crate::args::parse(["full", "x.pdf"].map(Into::into)).unwrap();
        let report_json = serde_json::json!({
            "schema": SCHEMA, "mode": "full", "file": "a.pdf", "file_bytes": 1,
            "engine": "e", "engine_version": "1", "status": "ok", "page_count": 1,
            "load_strategy": "read", "display_scale": 1.0, "tile_size": null, "workers": null,
            "repeat": 1, "read_ms": 0.1, "open_ms": open_ms, "metadata_ms": null,
            "first_page_ms": null, "time_to_first_page_ms": null, "samples_ms": null,
            "page_render_ms": null, "text_ms": null, "text_chars": null, "thumbnail_ms": null,
            "tiles": null, "rss_peak_mb": rss, "private_peak_mb": null, "cpu_ms": null,
            "page_errors": []
        });
        let mut r: FileReport = serde_json::from_value(report_json).unwrap();
        r.status = status;
        let _ = args;
        CorpusReport {
            schema: SCHEMA,
            created_unix: 0,
            machine: MachineInfo::default(),
            rustc: String::new(),
            profile: String::new(),
            git_rev: None,
            engine: "e".into(),
            display_scale: 1.0,
            tile_size: None,
            workers: None,
            runs_per_file: 1,
            results: vec![r],
        }
    }

    #[test]
    fn flags_slowdowns_beyond_threshold_and_ignores_noise() {
        let f = compare(
            &corpus(10.0, 80.0, Status::Ok),
            &corpus(12.0, 80.5, Status::Ok),
            10.0,
        );
        assert_eq!(f.len(), 1);
        assert!(f[0].regression && f[0].metric == "open_ms");
        // +5%: below threshold. RSS +0.5 MB: noise.
        assert!(
            compare(
                &corpus(10.0, 80.0, Status::Ok),
                &corpus(10.5, 80.5, Status::Ok),
                10.0
            )
            .is_empty()
        );
        // 0.2 ms -> 0.4 ms is +100% but below the noise floor.
        assert!(
            compare(
                &corpus(0.2, 80.0, Status::Ok),
                &corpus(0.4, 80.0, Status::Ok),
                10.0
            )
            .is_empty()
        );
    }

    #[test]
    fn status_degradation_is_a_regression() {
        let f = compare(
            &corpus(10.0, 80.0, Status::Ok),
            &corpus(10.0, 80.0, Status::Crash),
            10.0,
        );
        assert!(f.iter().any(|x| x.metric == "status" && x.regression));
        let g = compare(
            &corpus(10.0, 80.0, Status::Crash),
            &corpus(10.0, 80.0, Status::Ok),
            10.0,
        );
        assert!(g.iter().all(|x| !x.regression));
    }
}
