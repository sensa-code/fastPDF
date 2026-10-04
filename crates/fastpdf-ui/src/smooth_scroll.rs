//! Smooth scrolling for mouse-wheel notches (spec: scrolling must feel
//! smooth). A notch scrolls with a short ease-out instead of jumping three
//! lines at once; touchpads (fractional wheel deltas), the keyboard and
//! dragging stay immediate.
//!
//! The animation only advances in frames the viewport requests while it
//! runs. When it ends nothing requests another frame, so an idle window
//! stays idle (spec §1: idle CPU ~ 0).

use std::time::{Duration, Instant};

use gpui::ScrollDelta;

/// Length of one notch's animation.
pub(crate) const DURATION: Duration = Duration::from_millis(120);

/// A wheel scroll in progress: `total` logical pixels of view motion, of
/// which `applied` are done.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct SmoothScroll {
    total: (f32, f32),
    applied: (f32, f32),
    started: Option<Instant>,
}

impl SmoothScroll {
    pub(crate) fn is_active(&self) -> bool {
        self.started.is_some()
    }

    /// Adds a notch: what is left of the running animation plus `delta` is
    /// scrolled over a fresh [`DURATION`], so fast wheel turns accumulate
    /// instead of restarting from zero.
    pub(crate) fn add(&mut self, delta: (f32, f32), now: Instant) {
        let left = (self.total.0 - self.applied.0, self.total.1 - self.applied.1);
        self.total = (left.0 + delta.0, left.1 + delta.1);
        self.applied = (0.0, 0.0);
        self.started = Some(now);
    }

    /// The motion to apply in the frame drawn at `now` (ease-out cubic);
    /// the animation ends, exactly at its total, once its time is up.
    pub(crate) fn step(&mut self, now: Instant) -> Option<(f32, f32)> {
        let started = self.started?;
        let t = (now.saturating_duration_since(started).as_secs_f32() / DURATION.as_secs_f32())
            .clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - t).powi(3);
        let target = if t >= 1.0 {
            self.total
        } else {
            (self.total.0 * eased, self.total.1 * eased)
        };
        let step = (target.0 - self.applied.0, target.1 - self.applied.1);
        self.applied = target;
        if t >= 1.0 {
            *self = Self::default();
        }
        Some(step)
    }

    pub(crate) fn stop(&mut self) {
        *self = Self::default();
    }
}

/// Whether a wheel event is whole mouse-wheel notches. On Windows every
/// wheel message arrives as lines: a notch is a whole number of lines (the
/// system's lines per notch), a precision touchpad sends fractions.
pub(crate) fn is_wheel_notch(delta: &ScrollDelta) -> bool {
    match delta {
        ScrollDelta::Lines(lines) => {
            let whole = |v: f32| v.fract() == 0.0;
            (lines.x != 0.0 || lines.y != 0.0) && whole(lines.x) && whole(lines.y)
        }
        ScrollDelta::Pixels(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px};

    fn at(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    #[test]
    fn a_notch_eases_out_and_lands_exactly() {
        let start = Instant::now();
        let mut s = SmoothScroll::default();
        assert_eq!(s.step(start), None, "idle: nothing to do");
        s.add((0.0, 99.0), start);
        let mut moved = 0.0;
        let mut steps = Vec::new();
        for ms in [16, 33, 50, 66, 83, 100, 116, 133] {
            if let Some((_, dy)) = s.step(at(start, ms)) {
                moved += dy;
                steps.push(dy);
            }
        }
        assert!((moved - 99.0).abs() < 1e-3, "the whole notch: {moved}");
        assert!(!s.is_active(), "done after {DURATION:?}");
        assert!(
            steps[0] > steps[1] && steps[1] > steps[2],
            "fast start: {steps:?}"
        );
        assert!(steps[0] > 30.0, "a third of the way in the first frame");
        assert_eq!(s.step(at(start, 200)), None, "no frames once finished");
    }

    #[test]
    fn notches_during_a_scroll_accumulate() {
        let start = Instant::now();
        let mut s = SmoothScroll::default();
        s.add((0.0, 99.0), start);
        let first = s.step(at(start, 40)).map_or(0.0, |d| d.1);
        s.add((0.0, 99.0), at(start, 40));
        let mut moved = first;
        let mut ms = 40;
        while s.is_active() {
            ms += 16;
            moved += s.step(at(start, ms)).map_or(0.0, |d| d.1);
        }
        assert!((moved - 198.0).abs() < 1e-3, "{moved}");
        assert!(ms <= 40 + 136, "ends one duration after the last notch");
        s.add((0.0, -50.0), start);
        s.stop();
        assert!(!s.is_active());
    }

    #[test]
    fn only_whole_notches_are_smoothed() {
        assert!(is_wheel_notch(&ScrollDelta::Lines(point(0.0, -3.0))));
        assert!(is_wheel_notch(&ScrollDelta::Lines(point(6.0, 0.0))));
        assert!(
            !is_wheel_notch(&ScrollDelta::Lines(point(0.0, 0.2))),
            "touchpad"
        );
        assert!(!is_wheel_notch(&ScrollDelta::Lines(point(0.0, 0.0))));
        assert!(!is_wheel_notch(&ScrollDelta::Pixels(point(
            px(0.0),
            px(40.0)
        ))));
    }
}
