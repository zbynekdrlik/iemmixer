//! What the guard does when the engine exits (spec §2.4, §4.1; design §5.4).
//!
//! The guard never ends the engine itself: it only decides whether to start
//! it again, stay down with an alarm, or call it a crash loop, whose meaning
//! the lifecycle decides (`lifecycle::crash_loop`, S8).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum After {
    Respawn(Duration),
    /// 0 = asked to stop; 2/3 = configuration or card: respawning cannot help.
    Stay {
        alarm: Option<&'static str>,
    },
    /// A crash loop: `lifecycle::crash_loop` says where it goes (REAPER
    /// before the cutover, the pin or the previous pin after it).
    Loop,
}

/// Abnormal engine exits within the last [`CrashLoop::WINDOW`], and the
/// busy exits in a row.
#[derive(Debug, Clone, Default)]
pub struct CrashLoop {
    exits: VecDeque<Instant>,
    busy: usize,
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

    /// Counts an engine exit toward the busy streak: [`STATE_BUSY`] exits in
    /// a row, 0 after any other exit. The streak so far.
    pub fn busy(&mut self, busy: bool) -> usize {
        self.busy = if busy { self.busy + 1 } else { 0 };
        self.busy
    }
}

/// The engine's exit code when another process held its state directory
/// past its wait (`iem_engine::engine::STATE_WAIT`, 3 s; EX_TEMPFAIL):
/// most likely an engine that just ended and whose lock is not yet
/// released. No crash: tried again after [`BUSY_RETRY`] (#32 minor-4).
pub const STATE_BUSY: i32 = 75;

/// The delay before the engine starts again after [`STATE_BUSY`].
pub const BUSY_RETRY: Duration = Duration::from_secs(2);

/// Busy exits in a row after which the guard alarms (once per streak; it
/// keeps trying).
pub const BUSY_ALARM: usize = 3;

/// Busy exits in a row the guard tries again without counting a crash:
/// each takes the engine's 3 s wait and [`BUSY_RETRY`], so about 50 s. A
/// state directory still held then is held by something the guard does
/// not watch, so each further exit 75 counts as abnormal and the crash
/// loop's fallback (REAPER, or the previous pin in prod) runs (#32 F3-r4
/// 3).
pub const BUSY_LIMIT: usize = 10;

/// Whether an engine exit is a busy one tried again without a crash: exit
/// [`STATE_BUSY`] while the busy streak (this exit included) is at most
/// [`BUSY_LIMIT`].
pub fn busy_retry(code: Option<i32>, streak: usize) -> bool {
    code == Some(STATE_BUSY) && streak <= BUSY_LIMIT
}

/// Whether a plan's ready wait starts its engine again: once, when the
/// engine ended with [`STATE_BUSY`] before it was ready (`exit`: how it
/// ended, `None` while it runs; #32 F3-r4 4). Any other end, or a second
/// busy one, fails the step.
pub fn ready_restart(exit: Option<Option<i32>>, restarted: bool) -> bool {
    !restarted && exit == Some(Some(STATE_BUSY))
}

/// The respawn delay after the `n`-th abnormal exit in the window: 1, 2, 4,
/// 8 s, then 10 s.
pub fn backoff(abnormal_in_window: usize) -> Duration {
    let s = 1u64 << abnormal_in_window.saturating_sub(1).min(4);
    Duration::from_secs(s.min(10))
}

/// The engine's exit codes: 0 shut down, 1 i/o, 2 usage or site, 3 card
/// refused, 70 RT fault, 75 state directory busy ([`STATE_BUSY`]: tried
/// again while `busy_streak` allows, [`busy_retry`], then like a crash);
/// `None` when it ended without a code. The mode and the lifecycle decide
/// only what a loop means (`lifecycle::crash_loop`, S8).
pub fn after_exit(
    code: Option<i32>,
    session_ending: bool,
    looped: bool,
    n: usize,
    busy_streak: usize,
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
        _ if busy_retry(code, busy_streak) => After::Respawn(BUSY_RETRY),
        _ if looped => After::Loop,
        _ => After::Respawn(backoff(n)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        for looped in [false, true] {
            for ending in [false, true] {
                let after = |c| after_exit(Some(c), ending, looped, 1, 1);
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

    #[test]
    fn session_end_never_respawns() {
        for code in [None, Some(1), Some(70), Some(-1)] {
            for looped in [false, true] {
                assert_eq!(
                    after_exit(code, true, looped, 2, 1),
                    After::Stay { alarm: None },
                    "{code:?} {looped}"
                );
            }
        }
    }

    /// S8 (#11): a loop is a loop in every mode and lifecycle; where it goes
    /// (REAPER, the pin, the previous pin) is `lifecycle::crash_loop`'s.
    #[test]
    fn a_loop_is_named_for_the_lifecycle_to_decide() {
        for code in [None, Some(1), Some(70)] {
            assert_eq!(after_exit(code, false, true, 3, 1), After::Loop, "{code:?}");
        }
    }

    #[test]
    fn a_busy_state_directory_is_tried_again_after_2_s_without_a_crash() {
        // #32 minor-4: exit 75 = the engine waited for its state directory
        // (another engine's lock not yet released); no crash, so neither
        // the backoff nor a loop applies. A session ending still wins.
        for looped in [false, true] {
            for n in [0, 1, 3, 7] {
                assert_eq!(
                    after_exit(Some(75), false, looped, n, 1),
                    After::Respawn(Duration::from_secs(2)),
                    "{looped} {n}"
                );
            }
        }
        assert_eq!(
            after_exit(Some(75), true, false, 1, 1),
            After::Stay { alarm: None }
        );
    }

    #[test]
    fn a_busy_streak_beyond_ten_exits_counts_as_crashes() {
        // #32 F3-r4 3: up to BUSY_LIMIT busy exits in a row are tried again
        // after 2 s; the next one follows the backoff and the crash loop.
        assert_eq!(BUSY_LIMIT, 10);
        assert!(busy_retry(Some(75), 1) && busy_retry(Some(75), 10));
        assert!(!busy_retry(Some(75), 11));
        assert!(!busy_retry(Some(70), 1) && !busy_retry(None, 1));
        assert_eq!(
            after_exit(Some(75), false, false, 1, 10),
            After::Respawn(BUSY_RETRY)
        );
        // The backoff of the third abnormal exit (4 s), not BUSY_RETRY.
        assert_eq!(
            after_exit(Some(75), false, false, 3, 11),
            After::Respawn(Duration::from_secs(4))
        );
        assert_eq!(after_exit(Some(75), false, true, 3, 12), After::Loop);
        // A session ending still wins.
        assert_eq!(
            after_exit(Some(75), true, true, 3, 12),
            After::Stay { alarm: None }
        );
    }

    #[test]
    fn a_plans_ready_wait_starts_an_engine_again_only_after_one_busy_exit() {
        // #32 F3-r4 4: once, for exit 75; never for another end, nor for
        // an engine still running.
        assert!(ready_restart(Some(Some(75)), false));
        assert!(!ready_restart(Some(Some(75)), true));
        for exit in [
            None,
            Some(None),
            Some(Some(0)),
            Some(Some(70)),
            Some(Some(3)),
        ] {
            assert!(!ready_restart(exit, false), "{exit:?}");
        }
    }

    #[test]
    fn the_busy_streak_counts_busy_exits_in_a_row() {
        let mut c = CrashLoop::default();
        let got = [c.busy(true), c.busy(true), c.busy(false), c.busy(true)];
        assert_eq!(got, [1, 2, 0, 1]);
    }

    #[test]
    fn other_exits_respawn_after_the_backoff() {
        for code in [None, Some(1), Some(70), Some(-1073741819)] {
            assert_eq!(
                after_exit(code, false, false, 3, 1),
                After::Respawn(Duration::from_secs(4)),
                "{code:?}"
            );
        }
        assert_eq!(
            after_exit(None, false, false, 5, 1),
            After::Respawn(Duration::from_secs(10))
        );
    }
}
