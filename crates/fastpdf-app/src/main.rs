//! FastPDF desktop application.
//!
//! ```text
//! fastpdf [--engine NAME] [file.pdf]
//! ```
//!
//! Environment: `FASTPDF_LOG` (log filter, spec §31), `FASTPDF_ENGINE`
//! (engine name), `FASTPDF_BENCH=1` (start-up milestones as JSON lines on
//! stdout), `FASTPDF_DEV_OVERLAY=1` (development overlay, spec §46),
//! `FASTPDF_UPLOAD_BUDGET_MB` (per-frame texture upload budget; tuning),
//! `FASTPDF_RECENT_FILE` (recent-files list location; empty disables it, so
//! benchmarks and tests do not touch the user's list), `FASTPDF_SETTINGS_FILE`
//! (UI settings, `%APPDATA%\FastPDF\settings.toml` by default; empty: defaults
//! for this run and nothing written).
//!
//! Development switches, honored only in debug builds or together with
//! `FASTPDF_DEV_OVERLAY=1`: `FASTPDF_PRINT_TO_FILE=<path>` (every print job
//! writes into that file instead of reaching the printer; use it with the
//! "Microsoft Print to PDF" queue) and `FASTPDF_DEV_SCRIPT` (steps that
//! drive the reader without synthesized input; see `fastpdf_ui` devscript).
//!
//! Start-up order (spec §10, §29): the command-line document starts opening
//! on a background thread first thing in `main`, and when the window's size
//! can be predicted (saved placement, or the default window on the primary
//! monitor) its first page starts rendering there too, so engine work
//! overlaps GPUI's platform initialization and the first frame can show
//! exact tiles. Nothing waits on the engine.

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
use fastpdf_ui::{ReaderOptions, Startup};

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
    /// `Some(None)`: disabled; `Some(Some(path))`: custom location.
    recent_file: Option<Option<PathBuf>>,
    /// Same convention as `recent_file`.
    settings_file: Option<Option<PathBuf>>,
    print_to_file: Option<PathBuf>,
    dev_script: Option<String>,
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
            recent_file: std::env::var_os("FASTPDF_RECENT_FILE")
                .map(|v| (!v.is_empty()).then(|| PathBuf::from(v))),
            settings_file: std::env::var_os("FASTPDF_SETTINGS_FILE")
                .map(|v| (!v.is_empty()).then(|| PathBuf::from(v))),
            print_to_file: std::env::var_os("FASTPDF_PRINT_TO_FILE")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            dev_script: var("FASTPDF_DEV_SCRIPT"),
        }
    }

    /// Development switches apply to debug builds, or with the overlay on.
    fn development(&self) -> bool {
        cfg!(debug_assertions) || self.dev_overlay
    }
}

fn main() {
    let clock = bench::Clock::start();
    let env = Env::read();
    let development = env.development();
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
            "{USAGE}\n\nengines: {}\nenvironment: FASTPDF_LOG, FASTPDF_ENGINE, FASTPDF_BENCH, FASTPDF_DEV_OVERLAY, FASTPDF_UPLOAD_BUDGET_MB, FASTPDF_RECENT_FILE, FASTPDF_SETTINGS_FILE\ndevelopment: FASTPDF_PRINT_TO_FILE, FASTPDF_DEV_SCRIPT",
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

    let mut options = ReaderOptions::new(engine);
    options.system_ui_language = platform::ui_language();
    options.dev_overlay = env.dev_overlay;
    options.bench = env.bench.then(|| clock.hook());
    if let Some(recent) = env.recent_file {
        options.recent_files = recent;
    }
    if let Some(settings) = env.settings_file {
        options.settings_file = settings.map(|p| std::path::absolute(&p).unwrap_or(p));
    }
    if let Some(mb) = env.upload_budget_mb {
        options.upload_budget = mb.max(1).saturating_mul(1024 * 1024);
    }
    if development {
        if let Some(path) = env.print_to_file {
            let path = std::path::absolute(&path).unwrap_or(path);
            log::warn!("development: printing goes into {}", path.display());
            options.print_to_file = Some(path);
        }
        options.dev_script = env.dev_script;
    } else if env.print_to_file.is_some() || env.dev_script.is_some() {
        log::warn!(
            "FASTPDF_PRINT_TO_FILE / FASTPDF_DEV_SCRIPT ignored: release build without FASTPDF_DEV_OVERLAY=1"
        );
    }

    // Overlaps the engine's open, and page 1, with GPUI start-up (see the
    // module docs).
    let file = args
        .file
        .map(|path| std::path::absolute(&path).unwrap_or(path));
    let startup = Startup::begin(&options, file, platform::primary_screen());

    gpui_platform::application().run(move |cx| {
        fastpdf_ui::bind_keys(cx);
        match fastpdf_ui::open_reader_window(cx, options, startup) {
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

/// What the UI needs to know about the system before GPUI starts.
mod platform {
    use fastpdf_ui::ScreenGuess;

    /// The Windows UI language (a LANGID), which picks the UI text.
    #[cfg(windows)]
    #[allow(unsafe_code)]
    pub(crate) fn ui_language() -> Option<u16> {
        use windows_sys::Win32::Globalization::GetUserDefaultUILanguage;
        // SAFETY: no arguments, no pointers.
        let id = unsafe { GetUserDefaultUILanguage() };
        (id != 0).then_some(id)
    }

    /// The primary monitor as GPUI will see it — its size in physical
    /// pixels and its effective DPI — so the default window, and with it
    /// page 1's tiles, can be predicted. The process is per-monitor DPI
    /// aware from its manifest, so these are real pixels.
    #[cfg(windows)]
    #[allow(unsafe_code)]
    pub(crate) fn primary_screen() -> Option<ScreenGuess> {
        use windows_sys::Win32::Foundation::POINT;
        use windows_sys::Win32::Graphics::Gdi::{
            GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
        };
        use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

        // SAFETY: plain Win32 calls; the out-pointers are to locals of the
        // right type, and `cbSize` is set as GetMonitorInfoW requires.
        unsafe {
            let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
            if monitor.is_null() {
                return None;
            }
            let mut info: MONITORINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(monitor, &mut info) == 0 {
                return None;
            }
            let (mut dpi_x, mut dpi_y) = (0u32, 0u32);
            if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) != 0 {
                return None;
            }
            let r = info.rcMonitor;
            Some(ScreenGuess {
                width_px: u32::try_from(r.right - r.left).ok()?,
                height_px: u32::try_from(r.bottom - r.top).ok()?,
                scale: dpi_x as f32 / 96.0,
            })
        }
    }

    #[cfg(not(windows))]
    pub(crate) fn ui_language() -> Option<u16> {
        None
    }

    #[cfg(not(windows))]
    pub(crate) fn primary_screen() -> Option<ScreenGuess> {
        None
    }
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
