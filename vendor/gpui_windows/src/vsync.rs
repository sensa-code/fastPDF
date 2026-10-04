// Modified by FastPDF: Park the vsync thread while no window wants frames (0003). See FASTPDF-PATCHES.md.
use std::{
    sync::{
        LazyLock, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::Thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use gpui::FrameRequestSource;
use gpui_util::ResultExt;
use windows::Win32::{
    Foundation::HWND,
    Graphics::Dwm::{DWM_TIMING_INFO, DwmFlush, DwmGetCompositionTimingInfo},
    System::Performance::QueryPerformanceFrequency,
};

static QPC_TICKS_PER_SECOND: LazyLock<u64> = LazyLock::new(|| {
    let mut frequency = 0;
    // On systems that run Windows XP or later, the function will always succeed and
    // will thus never return zero.
    unsafe { QueryPerformanceFrequency(&mut frequency).unwrap() };
    frequency as u64
});

const VSYNC_INTERVAL_THRESHOLD: Duration = Duration::from_millis(1);
const DEFAULT_VSYNC_INTERVAL: Duration = Duration::from_micros(16_666); // ~60Hz

/// How long the vsync thread keeps ticking after the last frame demand before it
/// parks. It covers GPUI's one-second "keep presenting after high-rate input"
/// window and every place that relies on the next vsync to re-invalidate a
/// window (deferred re-entrant draws, presents of frames drawn during input).
pub(crate) const IDLE_FRAME_LINGER: Duration = Duration::from_secs(1);
/// While parked, the vsync thread still wakes this often to notice a lost GPU
/// device, so an idle window recovers after a driver reset without input.
pub(crate) const IDLE_DEVICE_CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// Whether any window wants frames, shared by the windows (UI thread) and the
/// vsync thread.
///
/// Windows report demand through GPUI's frame waker (a view was invalidated,
/// next-frame callbacks are pending) and for platform work that needs vsync
/// ticks (Direct Manipulation gestures, deferred draws). The vsync thread
/// consumes the demand on every tick and parks once there was none for
/// [`IDLE_FRAME_LINGER`], so an idle app stops waking up at the refresh rate.
pub(crate) struct FrameDemand {
    requested: AtomicBool,
    vsync_thread: Mutex<Option<Thread>>,
}

impl FrameDemand {
    pub(crate) fn new() -> Self {
        Self {
            requested: AtomicBool::new(false),
            vsync_thread: Mutex::new(None),
        }
    }

    /// Asks for frames, waking the vsync thread if it is parked.
    pub(crate) fn request(&self) {
        self.requested.store(true, Ordering::Release);
        self.wake();
    }

    /// Takes the pending demand; true if frames were requested since the last call.
    pub(crate) fn take(&self) -> bool {
        self.requested.swap(false, Ordering::AcqRel)
    }

    /// Wakes the vsync thread without asking for frames (e.g. to let it see its stop flag).
    pub(crate) fn wake(&self) {
        if let Some(thread) = self
            .vsync_thread
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            thread.unpark();
        }
    }

    /// Records the thread that [`Self::request`] wakes (the current vsync thread).
    pub(crate) fn set_vsync_thread(&self, thread: Thread) {
        *self.vsync_thread.lock().unwrap_or_else(|e| e.into_inner()) = Some(thread);
    }
}

/// Decides when the vsync thread may park: once there has been no frame demand
/// for `linger`.
pub(crate) struct IdleTracker {
    last_demand: Instant,
    linger: Duration,
}

impl IdleTracker {
    pub(crate) fn new(now: Instant, linger: Duration) -> Self {
        Self {
            last_demand: now,
            linger,
        }
    }

    /// Notes whether frames were requested at `now`; returns true when the
    /// thread should park.
    pub(crate) fn should_park(&mut self, demanded: bool, now: Instant) -> bool {
        if demanded {
            self.last_demand = now;
            return false;
        }
        now.saturating_duration_since(self.last_demand) >= self.linger
    }
}

pub(crate) struct VSyncProvider {
    interval: Duration,
    f: Box<dyn Fn() -> bool>,
}

impl VSyncProvider {
    pub(crate) fn new() -> Self {
        let interval = get_dwm_interval()
            .context("Failed to get DWM interval")
            .log_err()
            .unwrap_or(DEFAULT_VSYNC_INTERVAL);
        let f = Box::new(|| unsafe { DwmFlush().is_ok() });
        Self { interval, f }
    }

    pub(crate) fn wait_for_vsync(&self) -> FrameRequestSource {
        let vsync_start = Instant::now();
        let wait_succeeded = (self.f)();
        let elapsed = vsync_start.elapsed();
        // DwmFlush and DCompositionWaitForCompositorClock returns very early
        // instead of waiting until vblank when the monitor goes to sleep or is
        // unplugged (nothing to present due to desktop occlusion). We use 1ms as
        // a threshold for the duration of the wait functions and fallback to
        // Sleep() if it returns before that. This could happen during normal
        // operation for the first call after the vsync thread becomes non-idle,
        // but it shouldn't happen often.
        if !wait_succeeded || elapsed < VSYNC_INTERVAL_THRESHOLD {
            log::trace!("VSyncProvider::wait_for_vsync() took less time than expected");
            std::thread::sleep(self.interval);
            FrameRequestSource::LocalSchedule
        } else {
            FrameRequestSource::NativeCallback
        }
    }
}

fn get_dwm_interval() -> Result<Duration> {
    let mut timing_info = DWM_TIMING_INFO {
        cbSize: std::mem::size_of::<DWM_TIMING_INFO>() as u32,
        ..Default::default()
    };
    unsafe { DwmGetCompositionTimingInfo(HWND::default(), &mut timing_info) }?;
    let interval = retrieve_duration(timing_info.qpcRefreshPeriod, *QPC_TICKS_PER_SECOND);
    // Check for interval values that are impossibly low. A 29 microsecond
    // interval was seen (from a qpcRefreshPeriod of 60).
    if interval < VSYNC_INTERVAL_THRESHOLD {
        Ok(retrieve_duration(
            timing_info.rateRefresh.uiDenominator as u64,
            timing_info.rateRefresh.uiNumerator as u64,
        ))
    } else {
        Ok(interval)
    }
}

#[inline]
fn retrieve_duration(counts: u64, ticks_per_second: u64) -> Duration {
    let ticks_per_microsecond = ticks_per_second / 1_000_000;
    Duration::from_micros(counts / ticks_per_microsecond)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_compositor_wait_reports_native_callback() {
        let provider = VSyncProvider {
            interval: Duration::ZERO,
            f: Box::new(|| {
                std::thread::sleep(VSYNC_INTERVAL_THRESHOLD * 2);
                true
            }),
        };

        assert_eq!(
            provider.wait_for_vsync(),
            FrameRequestSource::NativeCallback
        );
    }

    #[test]
    fn failed_compositor_wait_reports_local_schedule_even_after_threshold() {
        let provider = VSyncProvider {
            interval: Duration::ZERO,
            f: Box::new(|| {
                std::thread::sleep(VSYNC_INTERVAL_THRESHOLD * 2);
                false
            }),
        };

        assert_eq!(provider.wait_for_vsync(), FrameRequestSource::LocalSchedule);
    }

    #[test]
    fn idle_tracker_parks_only_after_the_linger() {
        let start = Instant::now();
        let mut idle = IdleTracker::new(start, Duration::from_millis(100));
        assert!(!idle.should_park(false, start + Duration::from_millis(50)));
        assert!(!idle.should_park(true, start + Duration::from_millis(90)));
        assert!(
            !idle.should_park(false, start + Duration::from_millis(150)),
            "demand at 90 ms restarts the linger"
        );
        assert!(idle.should_park(false, start + Duration::from_millis(190)));
        assert!(!idle.should_park(true, start + Duration::from_millis(500)));
    }

    #[test]
    fn frame_demand_is_taken_once() {
        let demand = FrameDemand::new();
        assert!(!demand.take());
        demand.request();
        demand.request();
        assert!(demand.take());
        assert!(!demand.take());
    }

    #[test]
    fn frame_demand_wakes_a_parked_vsync_thread() {
        let demand = std::sync::Arc::new(FrameDemand::new());
        let (registered_tx, registered_rx) = std::sync::mpsc::channel();
        let (woke_tx, woke_rx) = std::sync::mpsc::channel();
        let thread_demand = demand.clone();
        let handle = std::thread::spawn(move || {
            thread_demand.set_vsync_thread(std::thread::current());
            registered_tx.send(()).unwrap();
            // Same shape as the vsync thread's park loop.
            while !thread_demand.take() {
                std::thread::park_timeout(Duration::from_secs(10));
            }
            woke_tx.send(Instant::now()).unwrap();
        });
        registered_rx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let requested = Instant::now();
        demand.request();
        let woke = woke_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(woke.duration_since(requested) < Duration::from_secs(1));
        handle.join().unwrap();
    }
}
