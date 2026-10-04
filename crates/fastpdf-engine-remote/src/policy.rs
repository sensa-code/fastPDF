//! Crash bookkeeping for one document (ADR 0008 §2), as pure logic.
//!
//! * When the host dies with exactly one request in flight, that request is
//!   the culprit: its page gets a definite strike.
//! * With several requests in flight nobody can tell which one crashed the
//!   host. Each gets a provisional strike and is retried *exclusively* (with
//!   nothing else admitted; the gate keeps an exclusive request's admission
//!   until the host has really finished it), so a second crash is attributed
//!   precisely. An exclusive retry that succeeds clears the provisional
//!   strike. Single-page geometry requests from the UI thread bypass the gate;
//!   one in flight next to an exclusive request makes a crash ambiguous
//!   rather than blamed on the wrong page.
//! * A page with `page_strikes` strikes (definite plus provisional; 2 by
//!   default) fails permanently and is never sent to a host again.
//! * When the parent kills a host because a request overran its deadline,
//!   that request is the culprit; the others are innocent bystanders.
//! * `storm_crashes` crashes within `storm_window` (3 in 60 s), or
//!   `max_total_crashes` in total, stop automatic restarts for the document.

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use crate::config::CrashPolicy;

/// What a request is about, for attributing crashes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Subject {
    Open,
    Page(u32),
    Metadata,
    Outline,
    /// Background geometry batches.
    Geometry,
}

/// How a host connection ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeathCause {
    /// Crashed, hit its memory limit, broke the protocol, stopped reading,
    /// or exited on its own.
    Crash,
    /// The parent terminated it because request `culprit` overran its
    /// deadline.
    Deadline { culprit: u64 },
    /// The document was closed.
    Shutdown,
}

/// A request that was in flight when the host died.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InFlight {
    pub(crate) id: u64,
    /// `None` for housekeeping (memory usage, trim): never blamed.
    pub(crate) subject: Option<Subject>,
}

/// What happens to a request whose host died.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Send it again on a new host, alone if `exclusive`.
    Retry { exclusive: bool },
    /// It brought the host down; report the failure.
    Culprit,
    /// Automatic restarts are off for this document.
    Disabled,
    /// The document is closing.
    Shutdown,
}

#[derive(Debug, Default, Clone, Copy)]
struct Strikes {
    definite: u8,
    provisional: bool,
}

impl Strikes {
    fn total(self) -> u8 {
        self.definite.saturating_add(u8::from(self.provisional))
    }
}

#[derive(Debug)]
pub(crate) struct Ledger {
    policy: CrashPolicy,
    strikes: HashMap<Subject, Strikes>,
    recent: VecDeque<Instant>,
    total: u32,
    disabled: bool,
}

impl Ledger {
    pub(crate) fn new(policy: CrashPolicy) -> Self {
        Self {
            policy,
            strikes: HashMap::new(),
            recent: VecDeque::new(),
            total: 0,
            disabled: false,
        }
    }

    pub(crate) fn is_disabled(&self) -> bool {
        self.disabled
    }

    pub(crate) fn total_crashes(&self) -> u32 {
        self.total
    }

    fn get(&self, s: Subject) -> Strikes {
        self.strikes.get(&s).copied().unwrap_or_default()
    }

    /// Never sent to a host again.
    pub(crate) fn is_permanent(&self, s: Subject) -> bool {
        self.get(s).total() >= self.policy.page_strikes.max(1)
    }

    /// Runs exclusively until cleared.
    pub(crate) fn is_suspect(&self, s: Subject) -> bool {
        self.get(s).total() > 0
    }

    /// Pages that failed permanently, in order.
    pub(crate) fn failed_pages(&self) -> Vec<u32> {
        let mut pages: Vec<u32> = self
            .strikes
            .iter()
            .filter(|(s, st)| {
                matches!(s, Subject::Page(_)) && st.total() >= self.policy.page_strikes.max(1)
            })
            .filter_map(|(s, _)| match s {
                Subject::Page(p) => Some(*p),
                _ => None,
            })
            .collect();
        pages.sort_unstable();
        pages
    }

    /// An exclusive retry of `s` succeeded: its provisional strike was a
    /// false suspicion.
    pub(crate) fn cleared(&mut self, s: Subject) {
        if let Some(st) = self.strikes.get_mut(&s) {
            st.provisional = false;
            if st.total() == 0 {
                self.strikes.remove(&s);
            }
        }
    }

    /// Records a host death and decides what happens to each request that
    /// was in flight.
    pub(crate) fn host_died(
        &mut self,
        now: Instant,
        cause: DeathCause,
        in_flight: &[InFlight],
    ) -> HashMap<u64, Verdict> {
        if cause == DeathCause::Shutdown {
            return in_flight
                .iter()
                .map(|f| (f.id, Verdict::Shutdown))
                .collect();
        }
        self.total = self.total.saturating_add(1);
        self.recent.push_back(now);
        while self
            .recent
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) > self.policy.storm_window)
        {
            self.recent.pop_front();
        }
        if self.recent.len() >= self.policy.storm_crashes as usize
            || self
                .policy
                .max_total_crashes
                .is_some_and(|max| self.total >= max)
        {
            self.disabled = true;
        }

        let blamable: Vec<&InFlight> = in_flight.iter().filter(|f| f.subject.is_some()).collect();
        let culprit = match cause {
            DeathCause::Deadline { culprit } => Some(culprit),
            DeathCause::Crash if blamable.len() == 1 => Some(blamable[0].id),
            DeathCause::Crash => None,
            DeathCause::Shutdown => None,
        };
        let mut verdicts = HashMap::with_capacity(in_flight.len());
        for f in in_flight {
            let verdict = if Some(f.id) == culprit {
                if let Some(s) = f.subject {
                    let st = self.strikes.entry(s).or_default();
                    st.definite = st.definite.saturating_add(1);
                }
                Verdict::Culprit
            } else {
                if cause == DeathCause::Crash
                    && culprit.is_none()
                    && let Some(s) = f.subject
                {
                    self.strikes.entry(s).or_default().provisional = true;
                }
                if self.disabled {
                    Verdict::Disabled
                } else {
                    Verdict::Retry {
                        exclusive: f.subject.is_some_and(|s| self.is_suspect(s)),
                    }
                }
            };
            verdicts.insert(f.id, verdict);
        }
        verdicts
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn page(id: u64, page: u32) -> InFlight {
        InFlight {
            id,
            subject: Some(Subject::Page(page)),
        }
    }

    #[test]
    fn a_lone_request_is_the_culprit_and_fails_permanently_on_its_second_crash() {
        let mut l = Ledger::new(CrashPolicy::default());
        let t = Instant::now();
        let v = l.host_died(t, DeathCause::Crash, &[page(1, 7)]);
        assert_eq!(v[&1], Verdict::Culprit);
        assert!(l.is_suspect(Subject::Page(7)) && !l.is_permanent(Subject::Page(7)));
        let v = l.host_died(
            t + Duration::from_secs(100),
            DeathCause::Crash,
            &[page(2, 7)],
        );
        assert_eq!(v[&2], Verdict::Culprit);
        assert!(l.is_permanent(Subject::Page(7)));
        assert_eq!(l.failed_pages(), vec![7]);
        assert!(!l.is_permanent(Subject::Page(8)));
    }

    #[test]
    fn several_in_flight_are_retried_alone_and_innocents_are_cleared() {
        let mut l = Ledger::new(CrashPolicy::default());
        let t = Instant::now();
        let v = l.host_died(t, DeathCause::Crash, &[page(1, 3), page(2, 4), page(3, 5)]);
        for id in 1..=3 {
            assert_eq!(v[&id], Verdict::Retry { exclusive: true });
        }
        // Page 4 crashes again when retried alone: second strike.
        let v = l.host_died(t + Duration::from_secs(1), DeathCause::Crash, &[page(4, 4)]);
        assert_eq!(v[&4], Verdict::Culprit);
        assert!(l.is_permanent(Subject::Page(4)));
        // Pages 3 and 5 render fine alone: suspicion lifted.
        l.cleared(Subject::Page(3));
        l.cleared(Subject::Page(5));
        assert!(!l.is_suspect(Subject::Page(3)) && !l.is_suspect(Subject::Page(5)));
        assert_eq!(l.failed_pages(), vec![4]);
    }

    #[test]
    fn housekeeping_requests_are_never_blamed() {
        let mut l = Ledger::new(CrashPolicy::default());
        let housekeeping = InFlight {
            id: 9,
            subject: None,
        };
        let v = l.host_died(
            Instant::now(),
            DeathCause::Crash,
            &[housekeeping, page(1, 2)],
        );
        assert_eq!(v[&1], Verdict::Culprit);
        assert_eq!(v[&9], Verdict::Retry { exclusive: false });
    }

    #[test]
    fn deadline_kills_blame_only_the_slow_request() {
        let mut l = Ledger::new(CrashPolicy::default());
        let v = l.host_died(
            Instant::now(),
            DeathCause::Deadline { culprit: 2 },
            &[page(1, 1), page(2, 9)],
        );
        assert_eq!(v[&2], Verdict::Culprit);
        assert_eq!(v[&1], Verdict::Retry { exclusive: false });
        assert!(l.is_suspect(Subject::Page(9)) && !l.is_suspect(Subject::Page(1)));
    }

    #[test]
    fn three_crashes_in_a_minute_stop_restarts() {
        let mut l = Ledger::new(CrashPolicy::default());
        let t = Instant::now();
        l.host_died(t, DeathCause::Crash, &[]);
        l.host_died(t + Duration::from_secs(20), DeathCause::Crash, &[]);
        assert!(!l.is_disabled());
        let v = l.host_died(
            t + Duration::from_secs(40),
            DeathCause::Crash,
            &[page(1, 1), page(2, 2)],
        );
        assert!(l.is_disabled());
        assert_eq!(v[&1], Verdict::Disabled);
        assert_eq!(l.total_crashes(), 3);
    }

    #[test]
    fn crashes_spread_out_only_hit_the_total_cap() {
        let mut l = Ledger::new(CrashPolicy::default());
        let t = Instant::now();
        for i in 0..4 {
            l.host_died(t + Duration::from_secs(61 * i), DeathCause::Crash, &[]);
            assert!(!l.is_disabled(), "disabled after {} crashes", i + 1);
        }
        l.host_died(t + Duration::from_secs(61 * 4), DeathCause::Crash, &[]);
        assert!(l.is_disabled());

        let mut unlimited = Ledger::new(CrashPolicy {
            max_total_crashes: None,
            ..CrashPolicy::default()
        });
        for i in 0..20 {
            unlimited.host_died(t + Duration::from_secs(61 * i), DeathCause::Crash, &[]);
        }
        assert!(!unlimited.is_disabled());
    }

    #[test]
    fn shutdown_is_not_a_crash() {
        let mut l = Ledger::new(CrashPolicy::default());
        let v = l.host_died(Instant::now(), DeathCause::Shutdown, &[page(1, 1)]);
        assert_eq!(v[&1], Verdict::Shutdown);
        assert_eq!(l.total_crashes(), 0);
        assert!(!l.is_suspect(Subject::Page(1)));
    }

    #[test]
    fn a_crash_with_a_page_info_beside_an_exclusive_render_is_ambiguous() {
        let mut l = Ledger::new(CrashPolicy::default());
        let t = Instant::now();
        // Page 3 is suspect from an earlier group crash.
        l.host_died(t, DeathCause::Crash, &[page(1, 3), page(2, 8)]);
        // Its exclusive retry crashes while the UI asked for page 5's size.
        let v = l.host_died(
            t + Duration::from_secs(90),
            DeathCause::Crash,
            &[page(3, 3), page(4, 5)],
        );
        assert_eq!(v[&3], Verdict::Retry { exclusive: true });
        assert_eq!(v[&4], Verdict::Retry { exclusive: true });
        assert!(l.is_suspect(Subject::Page(5)));
        assert_eq!(l.failed_pages(), Vec::<u32>::new());
    }
}
