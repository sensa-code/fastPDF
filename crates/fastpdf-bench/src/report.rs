//! JSON report types. `schema` is bumped whenever a field changes meaning so
//! `compare` never mixes incompatible runs.

use std::path::Path;
use std::process::ExitCode;

use fastpdf_engine_api::PdfEngine;
use serde::{Deserialize, Serialize};

use crate::args::Args;
use crate::metrics::{self, MachineInfo};

pub(crate) const SCHEMA: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Status {
    /// Every measured stage succeeded.
    Ok,
    /// The document opened but some pages failed (recorded in `page_errors`).
    Partial,
    /// The file could not be read from disk.
    LoadError,
    /// The engine refused the document (malformed, encrypted, limits).
    OpenError,
    /// The child process died without producing a report (corpus only).
    Crash,
    /// The child process exceeded the per-file timeout (corpus only).
    Timeout,
}

/// Summary statistics over a set of millisecond samples.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub(crate) struct Stats {
    pub(crate) count: usize,
    pub(crate) min: f64,
    pub(crate) median: f64,
    pub(crate) p95: f64,
    pub(crate) max: f64,
    pub(crate) mean: f64,
}

impl Stats {
    pub(crate) fn from_samples(samples: &[f64]) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }
        let mut v = samples.to_vec();
        v.sort_by(f64::total_cmp);
        let at = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
        Some(Self {
            count: v.len(),
            min: v[0],
            median: at(0.5),
            p95: at(0.95),
            max: v[v.len() - 1],
            mean: v.iter().sum::<f64>() / v.len() as f64,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct PageError {
    /// 1-based page number.
    pub(crate) page: u32,
    pub(crate) stage: String,
    pub(crate) error: String,
}

/// Measurements for one file. Times are milliseconds, memory is MiB.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct FileReport {
    pub(crate) schema: u32,
    pub(crate) mode: String,
    pub(crate) file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) expect: Option<String>,
    pub(crate) file_bytes: Option<u64>,
    pub(crate) engine: String,
    pub(crate) engine_version: String,
    pub(crate) status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
    pub(crate) page_count: Option<u32>,
    pub(crate) load_strategy: Option<String>,
    pub(crate) display_scale: f32,
    pub(crate) tile_size: Option<u32>,
    pub(crate) workers: Option<usize>,
    pub(crate) repeat: u32,
    /// Reading or mapping the file.
    pub(crate) read_ms: Option<f64>,
    /// Engine open: minimum parse needed for the page count.
    pub(crate) open_ms: Option<f64>,
    /// First page's geometry plus document metadata.
    pub(crate) metadata_ms: Option<f64>,
    /// Rendering page 1 at the display scale.
    pub(crate) first_page_ms: Option<f64>,
    /// read + open + metadata + first page: the engine-side part of "time
    /// to first visible page".
    pub(crate) time_to_first_page_ms: Option<f64>,
    /// `open`/`render` modes: the repeated measurement.
    pub(crate) samples_ms: Option<Stats>,
    /// `full` mode: sampled pages.
    pub(crate) page_render_ms: Option<Stats>,
    pub(crate) text_ms: Option<f64>,
    pub(crate) text_chars: Option<usize>,
    pub(crate) thumbnail_ms: Option<Stats>,
    pub(crate) tiles: Option<u64>,
    pub(crate) rss_peak_mb: Option<f64>,
    pub(crate) private_peak_mb: Option<f64>,
    pub(crate) cpu_ms: Option<f64>,
    pub(crate) page_errors: Vec<PageError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) stderr: Option<String>,
}

impl FileReport {
    pub(crate) fn new(mode: &str, file: &Path, engine: &dyn PdfEngine, args: &Args) -> Self {
        let info = engine.info();
        Self {
            schema: SCHEMA,
            mode: mode.to_owned(),
            file: file.to_string_lossy().replace('\\', "/"),
            category: None,
            expect: None,
            file_bytes: None,
            engine: info.name.to_owned(),
            engine_version: info.version.to_owned(),
            status: Status::Ok,
            error: None,
            page_count: None,
            load_strategy: None,
            display_scale: args.scale,
            tile_size: args.tile,
            workers: args.workers,
            repeat: args.repeat,
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

    /// Records peak memory and CPU time of this process.
    pub(crate) fn capture_process_metrics(&mut self) {
        if let Some(m) = metrics::memory() {
            // Peaks are updated lazily by the kernel; never report a peak
            // below the current value.
            self.rss_peak_mb = Some(mib(m.peak_working_set.max(m.working_set)));
            self.private_peak_mb = Some(mib(m.peak_private.max(m.private)));
        }
        self.cpu_ms = metrics::cpu_time().map(|d| d.as_secs_f64() * 1000.0);
    }

    pub(crate) fn exit_code(&self) -> ExitCode {
        match self.status {
            Status::Ok | Status::Partial => ExitCode::SUCCESS,
            _ => ExitCode::FAILURE,
        }
    }
}

/// Result of a `corpus` run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct CorpusReport {
    pub(crate) schema: u32,
    pub(crate) created_unix: u64,
    pub(crate) machine: MachineInfo,
    pub(crate) rustc: String,
    pub(crate) profile: String,
    pub(crate) git_rev: Option<String>,
    pub(crate) engine: String,
    pub(crate) display_scale: f32,
    pub(crate) tile_size: Option<u32>,
    pub(crate) workers: Option<usize>,
    pub(crate) runs_per_file: u32,
    pub(crate) results: Vec<FileReport>,
}

fn mib(bytes: u64) -> f64 {
    (bytes as f64 / (1024.0 * 1024.0) * 10.0).round() / 10.0
}

/// Milliseconds with microsecond resolution, for stable JSON.
pub(crate) fn ms(d: std::time::Duration) -> f64 {
    (d.as_secs_f64() * 1_000_000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_quantiles() {
        let s = Stats::from_samples(&[5.0, 1.0, 3.0, 2.0, 4.0]).unwrap();
        assert_eq!((s.min, s.median, s.max, s.mean), (1.0, 3.0, 5.0, 3.0));
        assert_eq!(s.p95, 5.0);
        assert!(Stats::from_samples(&[]).is_none());
    }

    #[test]
    fn ms_rounds_to_microseconds() {
        assert_eq!(ms(std::time::Duration::from_nanos(1_234_567)), 1.235);
    }
}
