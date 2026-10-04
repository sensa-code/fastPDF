//! Out-of-process rendering for FastPDF (ADR 0008).
//!
//! PDF engines parse hostile input. `GuardedDocument` contains panics, but a
//! stack overflow, an allocation failure, a fail-fast abort or a runaway
//! computation inside an engine still takes the whole process with it. This
//! crate moves the engine into a **render host** process:
//!
//! * [`run_host`] is the host's entry point. The application calls it from
//!   `main`, before any UI is initialized, when it was started as a host,
//!   and passes a factory that creates engines by name. This crate itself
//!   depends on no PDF engine.
//! * [`RemoteEngine`] implements `PdfEngine` in FastPDF; its documents
//!   implement `EngineDocument` by sending requests to their host. Callers
//!   wrap them in `open_guarded` as usual.
//!
//! Transport (Windows): commands and replies travel over two one-way named
//! pipes that the host opens by name (the parent checks the client's PID;
//! no handle is inherited); tile pixels go through fixed-size slots of a
//! shared memory section (renders larger than a slot get a section of their
//! own, so every render is done in one piece and is byte-identical to
//! in-process rendering); document bytes are shared as a read-only section.
//! Tile renders skip the pipes: request and reply travel in the slot's
//! control block, with a semaphore and one event per slot as the only
//! wake-ups (`win::channel`). Pixels travel in RGBA, the engines' own
//! order, and FastPDF swaps them to BGRA while copying them out of the
//! slot.
//! The host is created inside a job object with a memory limit and
//! `KILL_ON_JOB_CLOSE`. The wire format (`protocol` module) is hand-written,
//! versioned and validated field by field in both directions.
//!
//! Failure handling: every request has a deadline; a host that crashes,
//! exceeds its memory limit, breaks the protocol or hangs is terminated and
//! replaced transparently. A page whose rendering crashes the host twice
//! fails permanently, and three crashes within 60 s stop automatic restarts
//! for the document ([`CrashPolicy`]).
//!
//! Other platforms: [`RemoteEngine::new`] returns `EngineError::Unsupported`
//! and callers keep using the in-process engine.

#[cfg(windows)]
mod client;
#[cfg_attr(not(windows), allow(dead_code))]
mod config;
#[cfg_attr(not(windows), allow(dead_code))]
mod gate;
#[cfg(windows)]
mod geometry;
#[cfg(windows)]
mod host;
#[cfg_attr(not(windows), allow(dead_code))]
mod policy;
#[cfg_attr(not(windows), allow(dead_code))]
mod protocol;
#[cfg(windows)]
mod remote;
#[cfg(windows)]
mod win;

use std::ffi::OsString;
use std::process::ExitCode;

use fastpdf_engine_api::{PageIndex, PdfEngine};

pub use config::{
    CrashPolicy, DEFAULT_MEMORY_LIMIT, DEFAULT_RENDER_THREADS, DEFAULT_SLOT_BYTES, RemoteConfig,
};
#[cfg(windows)]
pub use remote::RemoteEngine;
#[cfg(not(windows))]
pub use unsupported::RemoteEngine;

/// Supervision state of one document's render host, for diagnostics (the
/// memory overlay shows the host's PID, private bytes and restarts).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HostStats {
    /// Process id of the running host; `None` while none runs.
    pub pid: Option<u32>,
    /// Committed private bytes of the host process.
    pub private_bytes: Option<u64>,
    /// Hosts started after the first one.
    pub restarts: u32,
    /// Host crashes, deadline kills included.
    pub crashes: u32,
    pub last_crash: Option<String>,
    /// Pages that failed permanently.
    pub failed_pages: Vec<PageIndex>,
    /// True once automatic restarts stopped (crash storm).
    pub disabled: bool,
    /// Pages whose geometry FastPDF does not have yet (fetched in the
    /// background after opening).
    pub geometry_missing: u32,
}

/// Entry point of a render host process.
///
/// `args` are the arguments that follow the application's own host marker
/// (whatever [`RemoteConfig::args`] put in front of them). `factory` returns
/// the engine the parent asked for by name, or `None` if it does not know
/// it. Returns when FastPDF closes the connection; the process should exit
/// with the returned code right away.
pub fn run_host<F>(factory: F, args: impl IntoIterator<Item = OsString>) -> ExitCode
where
    F: Fn(&str) -> Option<Box<dyn PdfEngine>>,
{
    #[cfg(windows)]
    {
        host::run(factory, args)
    }
    #[cfg(not(windows))]
    {
        let _ = (factory, args.into_iter().count());
        eprintln!("the FastPDF render host is only implemented on Windows");
        ExitCode::FAILURE
    }
}

#[cfg(not(windows))]
mod unsupported {
    use std::convert::Infallible;

    use fastpdf_engine_api::{
        DocumentSource, EngineDocument, EngineError, EngineInfo, OpenOptions, PdfEngine,
    };

    use crate::{HostStats, RemoteConfig};

    /// Out-of-process rendering is only implemented on Windows; `new`
    /// always fails here, so no value of this type exists.
    #[derive(Debug)]
    pub struct RemoteEngine {
        never: Infallible,
    }

    impl RemoteEngine {
        pub fn new(config: RemoteConfig) -> Result<Self, EngineError> {
            let _ = config;
            Err(EngineError::Unsupported(
                "out-of-process rendering is only implemented on Windows".into(),
            ))
        }

        pub fn start(config: RemoteConfig, expected: EngineInfo) -> Result<Self, EngineError> {
            let _ = expected;
            Self::new(config)
        }

        pub fn ready(&self) -> Result<(), EngineError> {
            match self.never {}
        }

        pub fn spare_pid(&self) -> Option<u32> {
            match self.never {}
        }

        pub fn host_stats(&self) -> Vec<HostStats> {
            match self.never {}
        }
    }

    impl PdfEngine for RemoteEngine {
        fn info(&self) -> EngineInfo {
            match self.never {}
        }

        fn open(
            &self,
            _source: DocumentSource,
            _options: &OpenOptions,
        ) -> Result<Box<dyn EngineDocument>, EngineError> {
            match self.never {}
        }
    }
}
