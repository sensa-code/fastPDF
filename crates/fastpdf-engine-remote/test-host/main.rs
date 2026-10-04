//! Test-only render host: [`run_host`] with the synthetic engine.
//!
//! * `fastpdf-remote-test-host --render-host <connection args>`: a render
//!   host, started by `RemoteEngine` in the integration tests.
//! * `fastpdf-remote-test-host --parent wait|exit`: opens a synthetic
//!   document through a `RemoteEngine` whose host is this same executable,
//!   prints `host-pid <pid>`, then waits to be killed (`wait`) or exits at
//!   once without running any destructor (`exit`).

#[path = "synthetic.rs"]
mod synthetic;

use std::io::Write;
use std::process::ExitCode;
use std::time::Duration;

use fastpdf_engine_api::{DocumentSource, OpenOptions, PageIndex, PdfEngine, SharedBytes};
use fastpdf_engine_remote::{RemoteConfig, RemoteEngine, run_host};

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let mode = args.next().and_then(|a| a.into_string().ok());
    match mode.as_deref() {
        Some("--render-host") => run_host(synthetic::factory, args),
        Some("--parent") => match parent(args.next().and_then(|a| a.into_string().ok())) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("parent mode failed: {e}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("usage: fastpdf-remote-test-host --render-host <args> | --parent wait|exit");
            ExitCode::from(2)
        }
    }
}

fn parent(mode: Option<String>) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut config = RemoteConfig::new(exe, synthetic::NAME);
    config.args = vec!["--render-host".into()];
    config.keep_spare = false;
    let engine = RemoteEngine::new(config).map_err(|e| e.to_string())?;
    let bytes = synthetic::Spec::new(3).bytes();
    let doc = engine
        .open(
            DocumentSource::from_bytes(SharedBytes::from_vec(bytes)),
            &OpenOptions::default(),
        )
        .map_err(|e| e.to_string())?;
    doc.page_info(PageIndex::FIRST).map_err(|e| e.to_string())?;
    let mut out = std::io::stdout().lock();
    for stats in engine.host_stats() {
        if let Some(pid) = stats.pid {
            writeln!(out, "host-pid {pid}").map_err(|e| e.to_string())?;
        }
    }
    out.flush().map_err(|e| e.to_string())?;
    drop(out);
    if mode.as_deref() == Some("exit") {
        // No destructor runs: only the job object can end the host.
        std::process::exit(0);
    }
    // The test kills this process; the host must follow.
    std::thread::sleep(Duration::from_secs(120));
    drop(doc);
    Ok(())
}
