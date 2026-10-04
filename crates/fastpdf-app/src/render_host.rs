//! Out-of-process rendering (ADR 0008), opt-in.
//!
//! `fastpdf --engine hayro-isolated` (or `FASTPDF_ENGINE=hayro-isolated`)
//! renders through a render host: this same executable, started as
//! `fastpdf --render-host ...`, inside a job object with a memory limit, so
//! an engine crash, allocation bomb or hang ends the host instead of the
//! reader. Without the suffix nothing changes: the engine runs in-process.

use std::ffi::OsStr;
use std::process::ExitCode;

use fastpdf_engine_api::PdfEngine;
use fastpdf_engine_remote::{RemoteConfig, RemoteEngine};

/// First argument of a render host process.
const HOST_FLAG: &str = "--render-host";

/// Suffix of the engine names that render out of process.
pub(crate) const ISOLATED_SUFFIX: &str = "-isolated";

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

/// `engine` (an in-process engine) moved into a render host. Falls back to
/// `engine` itself, with a warning, when no host can be started (other
/// platforms, a broken installation).
pub(crate) fn isolate(engine: Box<dyn PdfEngine>) -> Box<dyn PdfEngine> {
    let name = engine.info().name;
    let remote = std::env::current_exe()
        .map_err(|e| e.to_string())
        .and_then(|exe| {
            let mut config = RemoteConfig::new(exe, name);
            config.args = vec![HOST_FLAG.into()];
            RemoteEngine::new(config).map_err(|e| e.to_string())
        });
    match remote {
        Ok(remote) => {
            log::info!(
                "render host isolation on for {name} (spare host pid {:?})",
                remote.spare_pid()
            );
            Box::new(remote)
        }
        Err(e) => {
            log::warn!("render host unavailable ({e}); {name} renders in-process");
            engine
        }
    }
}
