//! What the guard does when the engine exits (spec §2.4, §4.1; design §5.4).
//!
//! The guard never ends the engine itself: it only decides whether to start
//! it again, stay down with an alarm, or leave iemmixer for REAPER.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::plan::Mode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum After {
    Respawn(Duration),
    /// 0 = asked to stop; 2/3 = configuration or card: respawning cannot help.
    Stay {
        alarm: Option<&'static str>,
    },
    /// Crash loop before cutover or in dev: back to REAPER.
    ToEvent,
    /// Crash loop in prod: the previous pin's engine.
    PreviousPin,
}

/// Abnormal engine exits within the last [`CrashLoop::WINDOW`].
#[derive(Debug, Clone, Default)]
pub struct CrashLoop {
    exits: VecDeque<Instant>,
}

impl CrashLoop {
    pub const WINDOW: Duration = Duration::from_secs(600);
    pub const LIMIT: usize = 3;

    /// Records an abnormal exit; whether this makes a loop.
    pub fn record(&mut self, now: Instant) -> bool {
        self.exits.push_back(now);
        while self
            .exits
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= Self::WINDOW)
        {
            self.exits.pop_front();
        }
        self.exits.len() >= Self::LIMIT
    }

    /// Abnormal exits in the window as of the last [`CrashLoop::record`]
    /// (the backoff's input).
    pub fn in_window(&self) -> usize {
        self.exits.len()
    }
}

/// The respawn delay after the `n`-th abnormal exit in the window: 1, 2, 4,
/// 8 s, then 10 s.
pub fn backoff(abnormal_in_window: usize) -> Duration {
    let s = 1u64 << abnormal_in_window.saturating_sub(1).min(4);
    Duration::from_secs(s.min(10))
}

/// The engine's exit codes: 0 shut down, 1 i/o, 2 usage or site, 3 card
/// refused, 70 RT fault; `None` when it ended without a code.
pub fn after_exit(
    code: Option<i32>,
    mode: Mode,
    prod: bool,
    session_ending: bool,
    looped: bool,
    n: usize,
) -> After {
    match code {
        Some(0) => After::Stay { alarm: None },
        Some(2) => After::Stay {
            alarm: Some("engine site or usage error"),
        },
        Some(3) => After::Stay {
            alarm: Some("the card refused the engine"),
        },
        _ if session_ending => After::Stay { alarm: None },
        _ if looped && mode == Mode::Live && prod => After::PreviousPin,
        _ if looped => After::ToEvent,
        _ => After::Respawn(backoff(n)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODES: [Mode; 3] = [Mode::Event, Mode::Dev, Mode::Live];

    #[test]
    fn backoff_doubles_up_to_ten_seconds() {
        let got: Vec<u64> = (0..=7).map(|n| backoff(n).as_secs()).collect();
        assert_eq!(got, [1, 1, 2, 4, 8, 10, 10, 10]);
        assert_eq!(backoff(usize::MAX), Duration::from_secs(10));
    }

    #[test]
    fn three_exits_within_the_window_make_a_loop() {
        let t0 = Instant::now();
        let mut c = CrashLoop::default();
        assert!(!c.record(t0));
        assert_eq!(c.in_window(), 1);
        assert!(!c.record(t0 + Duration::from_secs(1)));
        assert_eq!(c.in_window(), 2);
        assert!(c.record(t0 + Duration::from_secs(599)));
        assert_eq!(c.in_window(), 3);
    }

    #[test]
    fn an_exit_600_s_old_has_left_the_window() {
        let t0 = Instant::now();
        let mut c = CrashLoop::default();
        assert!(!c.record(t0));
        assert!(!c.record(t0 + Duration::from_secs(1)));
        // The first exit is exactly 600 s old: two in the window, no loop.
        assert!(!c.record(t0 + CrashLoop::WINDOW));
        assert_eq!(c.in_window(), 2);
        // Both older ones are gone 600 s after the second.
        assert!(!c.record(t0 + Duration::from_secs(601)));
        assert_eq!(c.in_window(), 2);
    }

    #[test]
    fn clean_and_hopeless_exits_never_respawn() {
        for mode in MODES {
            for prod in [false, true] {
                for looped in [false, true] {
                    for ending in [false, true] {
                        let after = |c| after_exit(Some(c), mode, prod, ending, looped, 1);
                        assert_eq!(after(0), After::Stay { alarm: None });
                        assert_eq!(
                            after(2),
                            After::Stay {
                                alarm: Some("engine site or usage error")
                            }
                        );
                        assert_eq!(
                            after(3),
                            After::Stay {
                                alarm: Some("the card refused the engine")
                            }
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn session_end_never_respawns() {
        for code in [None, Some(1), Some(70), Some(-1)] {
            for mode in MODES {
                for looped in [false, true] {
                    assert_eq!(
                        after_exit(code, mode, true, true, looped, 2),
                        After::Stay { alarm: None },
                        "{code:?} {mode:?} {looped}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_loop_goes_to_event_except_in_prod_live() {
        assert_eq!(
            after_exit(Some(70), Mode::Live, true, false, true, 3),
            After::PreviousPin
        );
        // A trial (live before cutover) and dev go back to REAPER.
        assert_eq!(
            after_exit(Some(70), Mode::Live, false, false, true, 3),
            After::ToEvent
        );
        assert_eq!(
            after_exit(Some(70), Mode::Dev, true, false, true, 3),
            After::ToEvent
        );
        assert_eq!(
            after_exit(None, Mode::Dev, false, false, true, 3),
            After::ToEvent
        );
        assert_eq!(
            after_exit(Some(1), Mode::Event, true, false, true, 3),
            After::ToEvent
        );
    }

    #[test]
    fn other_exits_respawn_after_the_backoff() {
        for code in [None, Some(1), Some(70), Some(-1073741819)] {
            for mode in MODES {
                for prod in [false, true] {
                    assert_eq!(
                        after_exit(code, mode, prod, false, false, 3),
                        After::Respawn(Duration::from_secs(4)),
                        "{code:?} {mode:?} {prod}"
                    );
                }
            }
        }
        assert_eq!(
            after_exit(None, Mode::Live, true, false, false, 5),
            After::Respawn(Duration::from_secs(10))
        );
    }
}
