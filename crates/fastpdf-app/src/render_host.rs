//! Out-of-process rendering (ADR 0008), opt-in.
//!
//! `fastpdf --engine hayro-isolated` (or `FASTPDF_ENGINE=hayro-isolated`)
//! renders through a render host: this same executable, started as
//! `fastpdf --render-host ...`, inside a job object with a memory limit, so
//! an engine crash, allocation bomb or hang ends the host instead of the
//! reader. Without the suffix nothing changes: the engine runs in-process.
//!
//! The host starts on a background thread while FastPDF itself starts up
//! (window, GPUI); only opening a document waits for it. Document files are
//! handed to the host as a read-only handle: FastPDF opens them but neither
//! reads nor maps them (`loader::set_handle_only`).

use std::ffi::OsStr;
use std::process::ExitCode;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use fastpdf_engine_api::{
    DocumentSource, EngineDocument, EngineError, EngineInfo, OpenOptions, PdfEngine,
};
use fastpdf_engine_remote::{RemoteConfig, RemoteEngine};

/// First argument of a render host process.
const HOST_FLAG: &str = "--render-host";

/// Suffix of the engine names that render out of process.
pub(crate) const ISOLATED_SUFFIX: &str = "-isolated";

/// Host memory limit in bytes; 0: the default (`RemoteConfig`).
static MEMORY_LIMIT: AtomicU64 = AtomicU64::new(0);

/// Development: overrides the render host's memory limit (crash testing
/// with hostile files, `FASTPDF_HOST_MEMORY_MB`). Call before `isolate`.
pub(crate) fn set_memory_limit(bytes: u64) {
    MEMORY_LIMIT.store(bytes, Ordering::Relaxed);
}

/// Runs the render host when this process was started as one. Must be the
/// first thing `main` does: a host loads no settings, opens no window and
/// does not initialize GPUI.
pub(crate) fn dispatch() -> Option<ExitCode> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(OsStr::new(HOST_FLAG)) {
        return None;
    }
    Some(fastpdf_engine_remote::run_host(
        crate::engines::create,
        args,
    ))
}

/// `engine` (an in-process engine) moved into a render host. Does not wait
/// for the host to start. Falls back to `engine` itself, with a warning,
/// when no host can be started (other platforms, a broken installation).
pub(crate) fn isolate(engine: Box<dyn PdfEngine>) -> Box<dyn PdfEngine> {
    let info = engine.info();
    let remote = std::env::current_exe()
        .map_err(|e| EngineError::Internal(format!("no executable path: {e}")))
        .and_then(|exe| {
            let mut config = RemoteConfig::new(exe, info.name);
            config.args = vec![HOST_FLAG.into()];
            let limit = MEMORY_LIMIT.load(Ordering::Relaxed);
            if limit > 0 {
                log::warn!("development: render host memory limit {} MiB", limit >> 20);
                config.memory_limit = Some(limit);
            }
            RemoteEngine::start(config, info.clone())
        });
    match remote {
        Ok(remote) => {
            fastpdf_core::loader::set_handle_only(true);
            log::info!("render host isolation on for {}", info.name);
            Box::new(Isolated {
                remote,
                local: engine,
                usable: OnceLock::new(),
            })
        }
        Err(e) => {
            log::warn!(
                "render host unavailable ({e}); {} renders in-process",
                info.name
            );
            engine
        }
    }
}

/// The isolated engine, or the in-process one when the render host could
/// not be started.
struct Isolated {
    remote: RemoteEngine,
    local: Box<dyn PdfEngine>,
    /// Whether the first host started; settled by the first open.
    usable: OnceLock<bool>,
}

impl std::fmt::Debug for Isolated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Isolated")
            .field("remote", &self.remote)
            .field("usable", &self.usable.get())
            .finish_non_exhaustive()
    }
}

impl PdfEngine for Isolated {
    fn info(&self) -> EngineInfo {
        self.local.info()
    }

    fn open(
        &self,
        source: DocumentSource,
        options: &OpenOptions,
    ) -> Result<Box<dyn EngineDocument>, EngineError> {
        let usable = *self.usable.get_or_init(|| match self.remote.ready() {
            Ok(()) => true,
            Err(e) => {
                log::warn!(
                    "render host unavailable ({e}); {} renders in-process",
                    self.local.info().name
                );
                false
            }
        });
        if usable {
            self.remote.open(source, options)
        } else {
            self.local.open(source, options)
        }
    }
}
