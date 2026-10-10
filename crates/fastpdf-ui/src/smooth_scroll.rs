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

/// Page mode: wheel travel past the edge of the shown row that turns the
/// page. A mouse turns one page per notch (99 px at the Windows default of
/// three lines). A touchpad sends a stream of small deltas and keeps going
/// with inertia, so it needs more travel, and after a turn its motion is
/// ignored for a moment.
const TURN_TRAVEL_NOTCH: f32 = 90.0;
const TURN_TRAVEL_TOUCHPAD: f32 = 160.0;
const TURN_PAUSE_TOUCHPAD: Duration = Duration::from_millis(400);

/// Collects wheel travel past the edge of the shown row (page mode).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct PageTurn {
    travel: f32,
    last_turn: Option<Instant>,
}

impl PageTurn {
    /// Adds `delta` logical pixels of view motion (positive: down) at the
    /// edge of the row; true when the travel adds up to a page turn in
    /// `delta`'s direction. `notch`: the delta came from a mouse wheel.
    pub(crate) fn push(&mut self, delta: f32, notch: bool, now: Instant) -> bool {
        if delta == 0.0 || !delta.is_finite() {
            return false;
        }
        if !notch
            && self
                .last_turn
                .is_some_and(|t| now.saturating_duration_since(t) < TURN_PAUSE_TOUCHPAD)
        {
            return false;
        }
        if self.travel * delta < 0.0 {
            self.travel = 0.0;
        }
        self.travel += delta;
        let needed = if notch {
            TURN_TRAVEL_NOTCH
        } else {
            TURN_TRAVEL_TOUCHPAD
        };
        if self.travel.abs() < needed {
            return false;
        }
        self.travel = 0.0;
        self.last_turn = Some(now);
        true
    }

    /// The view moved within the row: travel toward an edge starts over.
    pub(crate) fn reset(&mut self) {
        self.travel = 0.0;
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
    fn page_turns_need_a_notch_or_a_long_touchpad_swipe() {
        let start = Instant::now();
        let mut turn = PageTurn::default();
        assert!(turn.push(99.0, true, start), "one notch turns");
        assert!(turn.push(99.0, true, at(start, 10)), "every notch turns");
        assert!(turn.push(-99.0, true, at(start, 20)), "both ways");
        // A touchpad adds up its small deltas, then pauses.
        let mut turn = PageTurn::default();
        let swipes = (1..=20u64)
            .filter(|&i| turn.push(10.0, false, at(start, i * 10)))
            .count();
        assert_eq!(swipes, 1, "160 px of 200 turn once; the rest is paused");
        assert!(!turn.push(-200.0, false, at(start, 300)), "still paused");
        assert!(turn.push(-200.0, false, at(start, 700)));
        // Changing direction or moving inside the row starts over.
        let mut turn = PageTurn::default();
        assert!(!turn.push(60.0, true, start));
        assert!(!turn.push(-60.0, true, start));
        assert!(!turn.push(60.0, true, start));
        turn.reset();
        assert!(!turn.push(60.0, true, start));
        assert!(!turn.push(f32::NAN, true, start));
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
