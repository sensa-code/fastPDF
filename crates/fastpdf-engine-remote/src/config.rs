//! Configuration of [`RemoteEngine`](crate::RemoteEngine).

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use fastpdf_engine_api::ResourceLimits;

/// Default commit limit of one render host. Normal documents peak at
/// 75–92 MiB in the host (ADR 0008 §4); the limit only stops runaways.
pub const DEFAULT_MEMORY_LIMIT: u64 = 1536 * 1024 * 1024;

/// A 512² tile with its gutter (516² × 4 bytes) fits in one slot.
pub const DEFAULT_SLOT_BYTES: usize = 17 * 64 * 1024;

/// Pages a host renders at a time by default: FastPDF's render scheduler
/// has two workers (`SchedulerConfig::default_workers`, B-3/B-4), and every
/// render thread keeps engine caches of its own.
pub const DEFAULT_RENDER_THREADS: u32 = 2;

// A 512² tile with its 2 px gutter on every side fits in one slot.
const _: () = assert!(516 * 516 * 4 <= DEFAULT_SLOT_BYTES);

/// Grace period on top of the engine's own render-time budget before a
/// request is considered hung.
const DEADLINE_GRACE: Duration = Duration::from_secs(10);
/// Request deadline when the document has no render-time budget.
const DEFAULT_DEADLINE: Duration = Duration::from_secs(60);
/// Opening may legitimately take longer than one render.
const MIN_OPEN_DEADLINE: Duration = Duration::from_secs(30);

/// How a [`RemoteEngine`](crate::RemoteEngine) starts and supervises its
/// render hosts (one host per open document, ADR 0008 §2).
#[derive(Debug, Clone)]
pub struct RemoteConfig {
    /// Render host executable; FastPDF passes its own `current_exe()`.
    pub program: PathBuf,
    /// Arguments placed before the connection arguments, e.g.
    /// `["--render-host"]`. The host's `main` passes everything after them
    /// to [`run_host`](crate::run_host).
    pub args: Vec<OsString>,
    /// Engine name handed to the host's engine factory (`"hayro"`).
    pub engine: String,
    /// Commit limit per host process; `None` disables the limit.
    pub memory_limit: Option<u64>,
    /// Longest a request may run before its host is presumed hung and is
    /// terminated. `None`: the document's `max_render_time` plus 10 s (60 s
    /// without a render-time budget).
    pub request_timeout: Option<Duration>,
    /// Budget for spawning a host and completing the handshake.
    pub startup_timeout: Duration,
    /// Tile slots per host (shared memory); 0 sends every render through a
    /// section of its own.
    pub slots: u32,
    /// Size of one slot; larger renders get a section of their own.
    pub slot_bytes: usize,
    /// Request worker threads in each host for everything but renders and
    /// page geometry (opening, text layers, outline, links, metadata).
    pub host_workers: u32,
    /// Renders a host runs at a time: as many as the caller renders tiles
    /// at a time in-process, so the engine keeps as many caches as there.
    /// Renders beyond that (several schedulers at once, such as the
    /// viewport's and the thumbnails') wait in the host.
    pub render_threads: u32,
    /// Keep one started host in reserve, so opening a document or restarting
    /// after a crash does not wait for a process to start.
    pub keep_spare: bool,
    pub crash_policy: CrashPolicy,
}

/// When a page or a document stops being retried (ADR 0008 §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashPolicy {
    /// Host crashes attributed to one page after which that page fails
    /// permanently for the rest of the session.
    pub page_strikes: u8,
    /// Host crashes within [`Self::storm_window`] after which a document
    /// stops restarting its host.
    pub storm_crashes: u32,
    pub storm_window: Duration,
    /// Total host crashes after which a document stops restarting its host.
    pub max_total_crashes: Option<u32>,
}

impl Default for CrashPolicy {
    fn default() -> Self {
        Self {
            page_strikes: 2,
            storm_crashes: 3,
            storm_window: Duration::from_secs(60),
            max_total_crashes: Some(5),
        }
    }
}

impl RemoteConfig {
    /// Defaults for running `program` with engine `engine`.
    pub fn new(program: impl Into<PathBuf>, engine: impl Into<String>) -> Self {
        let cpus = std::thread::available_parallelism().map_or(4, |n| n.get());
        Self {
            program: program.into(),
            args: Vec::new(),
            engine: engine.into(),
            memory_limit: Some(DEFAULT_MEMORY_LIMIT),
            request_timeout: None,
            startup_timeout: Duration::from_secs(10),
            slots: 10,
            slot_bytes: DEFAULT_SLOT_BYTES,
            host_workers: u32::try_from(cpus.clamp(2, 8)).unwrap_or(4),
            render_threads: DEFAULT_RENDER_THREADS,
            keep_spare: true,
            crash_policy: CrashPolicy::default(),
        }
    }

    pub(crate) fn request_timeout(&self, limits: &ResourceLimits) -> Duration {
        self.request_timeout.unwrap_or_else(|| {
            limits
                .max_render_time
                .map_or(DEFAULT_DEADLINE, |t| t.saturating_add(DEADLINE_GRACE))
        })
    }

    pub(crate) fn open_timeout(&self, limits: &ResourceLimits) -> Duration {
        self.request_timeout(limits).max(MIN_OPEN_DEADLINE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadlines_follow_the_render_time_budget() {
        let mut config = RemoteConfig::new("host.exe", "test");
        let limits = ResourceLimits::default();
        assert_eq!(config.request_timeout(&limits), Duration::from_secs(30));
        let unlimited = ResourceLimits {
            max_render_time: None,
            ..ResourceLimits::default()
        };
        assert_eq!(config.request_timeout(&unlimited), DEFAULT_DEADLINE);
        config.request_timeout = Some(Duration::from_secs(2));
        assert_eq!(config.request_timeout(&limits), Duration::from_secs(2));
        assert_eq!(config.open_timeout(&limits), MIN_OPEN_DEADLINE);
    }
}
