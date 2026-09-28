//! Driver reopen budget and the stall rule (program spec §4.4; S6 design note
//! §3): at most one reopen per 5 minutes and three per process; a stall is no
//! callback for 2 s while the stream should run.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub const STALL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Reopen,
    /// Over budget: the stream faults and the guard respawns the engine.
    Fault,
}

#[derive(Debug, Clone)]
pub struct ResetBudget {
    window: Duration,
    per_window: usize,
    per_process: u32,
    used: u32,
    recent: VecDeque<Instant>,
}

impl Default for ResetBudget {
    fn default() -> Self {
        Self::new(Duration::from_secs(300), 1, 3)
    }
}

impl ResetBudget {
    pub fn new(window: Duration, per_window: usize, per_process: u32) -> Self {
        Self {
            window,
            per_window,
            per_process,
            used: 0,
            recent: VecDeque::new(),
        }
    }

    /// A reopen is wanted at `now`: allowed (and counted) or over budget.
    pub fn ask(&mut self, now: Instant) -> Verdict {
        while self
            .recent
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= self.window)
        {
            self.recent.pop_front();
        }
        if self.used >= self.per_process || self.recent.len() >= self.per_window {
            return Verdict::Fault;
        }
        self.used += 1;
        self.recent.push_back(now);
        Verdict::Reopen
    }

    /// Reopens granted so far.
    pub fn used(&self) -> u32 {
        self.used
    }
}

/// Whether the stream stalled: running, and no callback since `last` for `STALL`.
pub fn stalled(running: bool, last: Instant, now: Instant) -> bool {
    running && now.saturating_duration_since(last) >= STALL
}

#[cfg(test)]
mod tests {
    use super::Verdict::{Fault, Reopen};
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn one_reopen_per_five_minutes() {
        let t0 = Instant::now();
        let mut b = ResetBudget::default();
        assert_eq!(b.used(), 0);
        assert_eq!(b.ask(t0), Reopen);
        assert_eq!(b.ask(t0 + Duration::from_millis(299_999)), Fault);
        assert_eq!(b.used(), 1);
        // At exactly 300 s the first reopen has left the window.
        assert_eq!(b.ask(t0 + secs(300)), Reopen);
        assert_eq!(b.used(), 2);
        assert_eq!(b.ask(t0 + secs(301)), Fault);
    }

    #[test]
    fn three_reopens_per_process() {
        let t0 = Instant::now();
        let mut b = ResetBudget::default();
        let spaced = [b.ask(t0), b.ask(t0 + secs(301)), b.ask(t0 + secs(602))];
        assert_eq!(spaced, [Reopen; 3]);
        assert_eq!(b.used(), 3);
        assert_eq!(b.ask(t0 + secs(903)), Fault);
        assert_eq!(b.ask(t0 + secs(100_000)), Fault);
        assert_eq!(b.used(), 3);
    }

    #[test]
    fn a_budget_keeps_its_own_numbers() {
        let t0 = Instant::now();
        let mut b = ResetBudget::new(secs(10), 2, 5);
        assert_eq!(b.ask(t0), Reopen);
        assert_eq!(b.ask(t0 + secs(1)), Reopen);
        // Two per 10 s.
        assert_eq!(b.ask(t0 + secs(2)), Fault);
        // The first has left the window; the second has not.
        assert_eq!(b.ask(t0 + secs(10)), Reopen);
        assert_eq!(b.ask(t0 + Duration::from_millis(10_500)), Fault);
        assert_eq!(b.ask(t0 + secs(11)), Reopen);
        assert_eq!(b.ask(t0 + secs(30)), Reopen);
        assert_eq!(b.used(), 5);
        // Five per process.
        assert_eq!(b.ask(t0 + secs(60)), Fault);
        assert_eq!(b.used(), 5);
    }

    /// What the owner thread logs with every reopen and with an over-budget
    /// fault (#9 2026-09-28).
    #[test]
    fn the_budget_names_its_numbers() {
        let t0 = Instant::now();
        let mut b = ResetBudget::default();
        let fresh = BudgetState {
            used: 0,
            per_process: 3,
            recent: 0,
            per_window: 1,
            window: secs(300),
        };
        assert_eq!(b.state(), fresh);
        assert_eq!(
            fresh.to_string(),
            "reopens 0 of 3 per process, 0 of 1 within 300 s"
        );
        assert_eq!(b.ask(t0), Reopen);
        assert_eq!(
            b.state(),
            BudgetState {
                used: 1,
                recent: 1,
                ..fresh
            }
        );
        // Refused: nothing more is counted.
        assert_eq!(b.ask(t0 + secs(1)), Fault);
        assert_eq!(
            b.state().to_string(),
            "reopens 1 of 3 per process, 1 of 1 within 300 s"
        );
        // The first has left the window at the next ask.
        assert_eq!(b.ask(t0 + secs(300)), Reopen);
        assert_eq!(
            b.state(),
            BudgetState {
                used: 2,
                recent: 1,
                ..fresh
            }
        );
        let mut b = ResetBudget::new(secs(10), 2, 5);
        assert_eq!([b.ask(t0), b.ask(t0 + secs(1))], [Reopen; 2]);
        assert_eq!(
            b.state().to_string(),
            "reopens 2 of 5 per process, 2 of 2 within 10 s"
        );
        assert_eq!(b.ask(t0 + secs(12)), Reopen);
        assert_eq!(
            b.state(),
            BudgetState {
                used: 3,
                per_process: 5,
                recent: 1,
                per_window: 2,
                window: secs(10)
            }
        );
    }

    #[test]
    fn a_stall_is_two_seconds_without_a_callback_while_running() {
        assert_eq!(STALL, secs(2));
        let last = Instant::now();
        assert!(!stalled(true, last, last + Duration::from_millis(1_999)));
        assert!(stalled(true, last, last + secs(2)));
        assert!(stalled(true, last, last + secs(60)));
        assert!(!stalled(false, last, last + secs(3)));
        // A callback after the clock was read is no stall.
        assert!(!stalled(true, last + secs(5), last));
    }
}
