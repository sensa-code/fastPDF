//! `FASTPDF_BENCH=1`: one JSON line per start-up milestone on stdout
//! (benchmark plan B-8), timed from process creation so external scripts
//! can measure time to first visible page without screen scraping:
//!
//! ```text
//! {"event":"process_start","t_ms":0.000}
//! {"event":"main","t_ms":14.210}
//! {"event":"window_visible","t_ms":171.902}
//! {"event":"first_paint","t_ms":189.334}
//! {"event":"document_opened","t_ms":192.017}
//! {"event":"first_page_exact","t_ms":236.551}
//! ```

use std::io::Write;
use std::sync::Arc;
use std::time::Instant;

use fastpdf_ui::{BenchEvent, BenchHook};

#[derive(Debug, Clone, Copy)]
pub(crate) struct Clock {
    main_at: Instant,
    /// Process age when `main` started, from the OS process creation time.
    main_age_ms: f64,
}

impl Clock {
    /// Call first thing in `main`.
    pub(crate) fn start() -> Self {
        Self {
            main_at: Instant::now(),
            main_age_ms: process_age_ms().unwrap_or(0.0),
        }
    }

    /// Milliseconds since the process was created.
    pub(crate) fn now_ms(&self) -> f64 {
        self.main_age_ms + self.main_at.elapsed().as_secs_f64() * 1000.0
    }

    pub(crate) fn emit(&self, event: &str, t_ms: f64) {
        let _ = writeln!(
            std::io::stdout().lock(),
            "{{\"event\":\"{event}\",\"t_ms\":{t_ms:.3}}}"
        );
    }

    /// Prints the process start and `main` milestones.
    pub(crate) fn emit_startup(&self) {
        self.emit("process_start", 0.0);
        self.emit("main", self.main_age_ms);
    }

    pub(crate) fn hook(self) -> BenchHook {
        Arc::new(move |event: BenchEvent| self.emit(event.name(), self.now_ms()))
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn process_age_ms() -> Option<f64> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: pseudo-handle of the current process and four writable FILETIMEs.
    let ok = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    };
    if ok == 0 {
        return None;
    }
    let mut now = FILETIME::default();
    // SAFETY: writes one FILETIME.
    unsafe { GetSystemTimePreciseAsFileTime(&mut now) };
    // FILETIME counts 100 ns intervals.
    Some(ticks(now).saturating_sub(ticks(creation)) as f64 / 10_000.0)
}

#[cfg(not(windows))]
fn process_age_ms() -> Option<f64> {
    None
}
