//! `FASTPDF_BENCH=1`: one JSON line per start-up milestone on stdout
//! (benchmark plan B-8), timed from process creation so external scripts
//! can measure time to first visible page without screen scraping:
//!
//! ```text
//! {"event":"process_start","t_ms":0.000}
//! {"event":"main","t_ms":14.210}
//! {"event":"frame_counters","address":"0x7ff6d2c41a08","layout":"fastpdf-frame-counters/1"}
//! {"event":"window_visible","t_ms":171.902}
//! {"event":"first_paint","t_ms":189.334}
//! {"event":"document_opened","t_ms":192.017}
//! {"event":"first_page_exact","t_ms":236.551}
//! ```
//!
//! Frame events (render, prepaint, paint, wake) are counted instead of
//! printed, in [`FrameCounters`] at the address the `frame_counters` line
//! gives. `tools/bench-app -AppProbe` reads them with `ReadProcessMemory`
//! before and after an idle window to check that an idle reader draws
//! nothing: reading costs the reader nothing, and there is no thread,
//! timer, event or output that could wake it. Without `FASTPDF_BENCH` no
//! hook exists, so nothing is counted or printed and the counters stay zero.

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use fastpdf_ui::{BenchEvent, BenchHook};

/// Layout name printed with the counters' address; change it together with
/// [`FrameCounters`] and the reader in `tools/bench-app/BenchApp.cs`.
const FRAME_COUNTERS_LAYOUT: &str = "fastpdf-frame-counters/1";
/// First 8 bytes of [`FrameCounters`], checked by the external reader.
const FRAME_COUNTERS_MAGIC: [u8; 8] = *b"FPDFFRC1";

/// Frame event counters in a fixed layout for a reader outside the process:
/// the magic, then render, prepaint, paint and wake as native-endian `u64`
/// (little-endian on Windows), 40 bytes in all.
#[repr(C)]
pub(crate) struct FrameCounters {
    // Read only from outside the process (tools/bench-app), never by Rust.
    #[allow(dead_code)]
    magic: [u8; 8],
    render: AtomicU64,
    prepaint: AtomicU64,
    paint: AtomicU64,
    wake: AtomicU64,
}

impl FrameCounters {
    /// `const`: the process's counters are a plain static, nothing is
    /// allocated or started for them.
    pub(crate) const fn new() -> Self {
        Self {
            magic: FRAME_COUNTERS_MAGIC,
            render: AtomicU64::new(0),
            prepaint: AtomicU64::new(0),
            paint: AtomicU64::new(0),
            wake: AtomicU64::new(0),
        }
    }

    /// Counts a frame event; returns false for milestones, which are
    /// printed instead.
    fn count(&self, event: BenchEvent) -> bool {
        let counter = match event {
            BenchEvent::Render => &self.render,
            BenchEvent::Prepaint => &self.prepaint,
            BenchEvent::Paint => &self.paint,
            BenchEvent::Wake => &self.wake,
            BenchEvent::WindowVisible
            | BenchEvent::FirstPaint
            | BenchEvent::DocumentOpened
            | BenchEvent::FirstPageExact => return false,
        };
        counter.fetch_add(1, Ordering::Relaxed);
        true
    }

    fn address(&self) -> usize {
        std::ptr::from_ref(self).addr()
    }

    #[cfg(test)]
    fn snapshot(&self) -> [u64; 4] {
        [&self.render, &self.prepaint, &self.paint, &self.wake].map(|c| c.load(Ordering::Relaxed))
    }
}

/// The process's frame counters; only the bench hook touches them.
static FRAME_COUNTERS: FrameCounters = FrameCounters::new();

/// The startup line that tells the external reader where the counters are.
fn frame_counters_line(address: usize) -> String {
    format!(
        "{{\"event\":\"frame_counters\",\"address\":\"{address:#x}\",\"layout\":\"{FRAME_COUNTERS_LAYOUT}\"}}"
    )
}

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

    /// Prints the process start and `main` milestones, and where the frame
    /// counters are.
    pub(crate) fn emit_startup(&self) {
        self.emit("process_start", 0.0);
        self.emit("main", self.main_age_ms);
        let _ = writeln!(
            std::io::stdout().lock(),
            "{}",
            frame_counters_line(FRAME_COUNTERS.address())
        );
    }

    /// Prints milestones and counts frame events (see the module docs).
    pub(crate) fn hook(self) -> BenchHook {
        Arc::new(move |event: BenchEvent| {
            if !FRAME_COUNTERS.count(event) {
                self.emit(event.name(), self.now_ms());
            }
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn frame_events_are_counted_and_milestones_are_not() {
        let counters = FrameCounters::new();
        for event in [
            BenchEvent::Render,
            BenchEvent::Render,
            BenchEvent::Prepaint,
            BenchEvent::Paint,
            BenchEvent::Wake,
        ] {
            assert!(counters.count(event), "{event:?} is counted");
        }
        for event in [
            BenchEvent::WindowVisible,
            BenchEvent::FirstPaint,
            BenchEvent::DocumentOpened,
            BenchEvent::FirstPageExact,
        ] {
            assert!(!counters.count(event), "{event:?} is printed instead");
        }
        assert_eq!(counters.snapshot(), [2, 1, 1, 1]);
    }

    #[test]
    fn nothing_is_created_or_counted_without_a_hook() {
        // A `static` needs a `const` initializer: the counters can never
        // allocate, and creating them starts nothing.
        static UNUSED: FrameCounters = FrameCounters::new();
        assert_eq!(UNUSED.snapshot(), [0; 4]);
        // Only `Clock::hook` counts into the process's counters, and only
        // `FASTPDF_BENCH=1` creates a hook; this test binary has none.
        assert_eq!(FRAME_COUNTERS.snapshot(), [0; 4]);
        assert_eq!(UNUSED.magic, FRAME_COUNTERS_MAGIC);
    }

    #[test]
    fn layout_matches_the_external_reader() {
        // tools/bench-app/BenchApp.cs (ReadFrameCounters) reads these offsets.
        assert_eq!(offset_of!(FrameCounters, magic), 0);
        assert_eq!(offset_of!(FrameCounters, render), 8);
        assert_eq!(offset_of!(FrameCounters, prepaint), 16);
        assert_eq!(offset_of!(FrameCounters, paint), 24);
        assert_eq!(offset_of!(FrameCounters, wake), 32);
        assert_eq!(size_of::<FrameCounters>(), 40);
        assert_eq!(&FRAME_COUNTERS_MAGIC, b"FPDFFRC1");
    }

    #[test]
    fn the_startup_line_gives_the_address_in_hex() {
        let line = frame_counters_line(0x7ff6_d2c4_1a08);
        assert_eq!(
            line,
            r#"{"event":"frame_counters","address":"0x7ff6d2c41a08","layout":"fastpdf-frame-counters/1"}"#
        );
        assert_eq!(FRAME_COUNTERS.address() % 8, 0, "aligned for 8-byte reads");
    }
}
