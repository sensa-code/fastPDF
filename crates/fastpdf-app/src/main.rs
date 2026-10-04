//! FastPDF desktop application.
//!
//! ```text
//! fastpdf [--engine NAME] [file.pdf]
//! ```
//!
//! Environment: `FASTPDF_LOG` (log filter, spec §31), `FASTPDF_ENGINE`
//! (engine name), `FASTPDF_BENCH=1` (start-up milestones as JSON lines on
//! stdout), `FASTPDF_DEV_OVERLAY=1` (development overlay, spec §46),
//! `FASTPDF_UPLOAD_BUDGET_MB` (per-frame texture upload budget; tuning).
//!
//! Start-up order (spec §10): the command-line document starts opening on a
//! background thread first thing in `main`, so engine work overlaps GPUI's
//! platform initialization; the window appears as soon as GPUI is up and
//! shows the document when the open finishes. Nothing waits on the engine.

// Release builds are GUI-subsystem executables (no console window).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod bench;
mod engines;
mod logger;
#[cfg(feature = "engine-synthetic")]
mod synthetic;

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use fastpdf_engine_api::PdfEngine;
use fastpdf_ui::{PendingOpen, ReaderOptions};

const USAGE: &str = "usage: fastpdf [--engine NAME] [file.pdf]";

#[derive(Debug, Default, PartialEq)]
struct Args {
    file: Option<PathBuf>,
    engine: Option<String>,
    help: bool,
    version: bool,
}

impl Args {
    fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut parsed = Self::default();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("-h" | "--help") => parsed.help = true,
                Some("-V" | "--version") => parsed.version = true,
                Some("--engine") => {
                    let name = args
                        .next()
                        .and_then(|v| v.into_string().ok())
                        .ok_or("--engine needs a name")?;
                    parsed.engine = Some(name);
                }
                Some(flag) if flag.starts_with("--engine=") => {
                    parsed.engine = Some(flag["--engine=".len()..].to_owned());
                }
                Some(flag) if flag.starts_with('-') && flag.len() > 1 => {
                    return Err(format!("unknown option {flag}"));
                }
                _ => {
                    if parsed.file.is_some() {
                        return Err("only one file can be opened".into());
                    }
                    parsed.file = Some(PathBuf::from(arg));
                }
            }
        }
        Ok(parsed)
    }
}

/// Environment switches.
#[derive(Debug, Default)]
struct Env {
    log: Option<String>,
    engine: Option<String>,
    bench: bool,
    dev_overlay: bool,
    upload_budget_mb: Option<usize>,
}

impl Env {
    fn read() -> Self {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let flag = |name: &str| var(name).is_some_and(|v| v.trim() != "0");
        Self {
            log: var("FASTPDF_LOG"),
            engine: var("FASTPDF_ENGINE"),
            bench: flag("FASTPDF_BENCH"),
            dev_overlay: flag("FASTPDF_DEV_OVERLAY"),
            upload_budget_mb: var("FASTPDF_UPLOAD_BUDGET_MB").and_then(|v| v.trim().parse().ok()),
        }
    }
}

fn main() {
    let clock = bench::Clock::start();
    let env = Env::read();
    if env.log.is_some() || env.bench {
        console::attach_to_parent();
    }
    logger::init(env.log.as_deref());
    install_panic_hook();

    let args = match Args::parse(std::env::args_os().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("fastpdf: {message}\n{USAGE}");
            std::process::exit(2);
        }
    };
    if args.help {
        println!(
            "{USAGE}\n\nengines: {}\nenvironment: FASTPDF_LOG, FASTPDF_ENGINE, FASTPDF_BENCH, FASTPDF_DEV_OVERLAY",
            engines::names().join(", ")
        );
        return;
    }
    if args.version {
        println!("fastpdf {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if env.bench {
        clock.emit_startup();
    }

    let engine_name = args.engine.or(env.engine);
    let engine: Arc<dyn PdfEngine> = match engines::select(engine_name.as_deref()) {
        Ok(engine) => Arc::from(engine),
        Err(message) => {
            log::error!("{message}");
            eprintln!("fastpdf: {message}");
            std::process::exit(2);
        }
    };
    log::info!("engine: {}", engine.info().name);

    // Overlaps the engine's open with GPUI start-up (see module docs).
    let pending = args.file.map(|path| {
        PendingOpen::spawn(
            Arc::clone(&engine),
            std::path::absolute(&path).unwrap_or(path),
        )
    });

    let mut options = ReaderOptions::new(engine);
    options.dev_overlay = env.dev_overlay;
    options.bench = env.bench.then(|| clock.hook());
    if let Some(mb) = env.upload_budget_mb {
        options.upload_budget = mb.max(1).saturating_mul(1024 * 1024);
    }

    gpui_platform::application().run(move |cx| {
        fastpdf_ui::bind_keys(cx);
        match fastpdf_ui::open_reader_window(cx, options, pending) {
            Ok(_) => cx.activate(true),
            Err(e) => {
                log::error!("cannot open the main window: {e:#}");
                cx.quit();
            }
        }
    });
}

/// Panics are logged, never turned into aborts: engine panics are already
/// contained by `GuardedDocument`, and the release profile unwinds.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("<unnamed>");
        log::error!("panic on thread '{name}': {info}");
        let backtrace = std::backtrace::Backtrace::capture();
        if backtrace.status() == std::backtrace::BacktraceStatus::Captured {
            log::error!("{backtrace}");
        }
    }));
}

mod console {
    /// Release builds have no console of their own; when logging or bench
    /// output is requested from a terminal, write to that terminal. Already
    /// redirected handles (pipes, files) are left alone.
    #[cfg(all(windows, not(debug_assertions)))]
    #[allow(unsafe_code)]
    pub(crate) fn attach_to_parent() {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Console::{
            ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
        };
        // SAFETY: plain Win32 calls without pointers.
        unsafe {
            let unset = |h: windows_sys::Win32::Foundation::HANDLE| {
                h.is_null() || h == INVALID_HANDLE_VALUE
            };
            if unset(GetStdHandle(STD_OUTPUT_HANDLE)) && unset(GetStdHandle(STD_ERROR_HANDLE)) {
                AttachConsole(ATTACH_PARENT_PROCESS);
            }
        }
    }

    #[cfg(not(all(windows, not(debug_assertions))))]
    pub(crate) fn attach_to_parent() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        Args::parse(args.iter().map(OsString::from))
    }

    #[test]
    fn parses_file_and_engine() {
        let a = parse(&["--engine", "zpdf", r"C:\文件\報告.pdf"]).expect("valid");
        assert_eq!(a.engine.as_deref(), Some("zpdf"));
        assert_eq!(a.file, Some(PathBuf::from(r"C:\文件\報告.pdf")));
        let b = parse(&["--engine=hayro"]).expect("valid");
        assert_eq!(b.engine.as_deref(), Some("hayro"));
        assert_eq!(b.file, None);
    }

    #[test]
    fn rejects_bad_arguments() {
        assert!(parse(&["--bogus"]).is_err());
        assert!(parse(&["--engine"]).is_err());
        assert!(parse(&["a.pdf", "b.pdf"]).is_err());
        assert!(parse(&["-"]).is_ok_and(|a| a.file.is_some()));
    }
}
