//! Decisions of the ASIO backend's owner thread (S6 design note §3), portable
//! and mutation-tested. `asio.rs` (Windows only, excluded from mutation)
//! observes the driver, calls these with what it saw and acts on the answer:
//!
//! - [`frames`]: the configured buffer, checked before anything is written;
//! - [`open_period`]: the period an open measures from its first callbacks'
//!   sample positions ([`crate::period`]);
//! - [`Watchdog`] and [`reset_step`]: the stall clock and the reopen request
//!   put to the [`ResetBudget`] ([`crate::reset`]), its reasons ([`Asked`])
//!   logged with the verdict;
//! - [`session_end_release`], [`seh_step`] and [`stop_step`]: the bounded
//!   waits that end in a released (or parked) driver;
//! - [`seh_release`] and [`seh_faults`]: the owner's answer to a structured
//!   exception, and whether it is a fault, under the parked-engine test's
//!   hold (#35) or without it.

use std::fmt;
use std::time::{Duration, Instant};

use crate::period::{self, PeriodVerdict};
use crate::reset::{self, ResetBudget, Verdict};

/// Sample positions the callback records per open: its first callbacks
/// (11 ms at 32 samples). Any two bad deltas among them (two missed buffers,
/// or one late position) still leave [`NEED`] agreeing ones in a row.
pub const RING: usize = 32;
/// Agreeing consecutive deltas that decide the period.
pub const NEED: usize = 8;
/// An open whose period is still undecided this long after `start()` fails.
pub const DECIDE_WITHIN: Duration = Duration::from_secs(1);
/// At the end of the Windows session the owner thread releases the driver
/// once the engine asked the stream to stop (it saved and faded), and after
/// this long at the latest.
pub const SESSION_END_WAIT: Duration = Duration::from_secs(3);
/// The SEH filter waits this long for the driver's release (design §3).
pub const SEH_WAIT: Duration = Duration::from_secs(1);
/// `AsioStream::stop` waits this long for the owner thread's outcome.
pub const STOP_BOUND: Duration = Duration::from_secs(5);

/// The configured buffer as a sample count: positive, or refused before the
/// preference window writes anything.
pub fn frames(configured: i32) -> Option<u32> {
    u32::try_from(configured).ok().filter(|f| *f > 0)
}

/// What an open does after one look at the period ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenPeriod {
    /// Not decided yet: wait for more callbacks.
    Wait,
    /// The measured period (frames per callback), equal to the expected one.
    Ok(u32),
    /// The open fails (exit 3): another period, or none decided within the
    /// ring or [`DECIDE_WITHIN`].
    Refuse(PeriodVerdict),
}

/// The ring's runs of consecutive callbacks with a position (a negative
/// position counts as none): only consecutive callbacks give deltas.
fn runs(ring: &[Option<i64>]) -> Vec<Vec<u64>> {
    let valid: Vec<Option<u64>> = ring
        .iter()
        .map(|p| p.and_then(|v| u64::try_from(v).ok()))
        .collect();
    valid
        .split(Option::is_none)
        .map(|run| run.iter().flatten().copied().collect())
        .collect()
}

/// The period of every [`NEED`] consecutive agreeing deltas in the ring, in
/// ring order.
fn agreeing(ring: &[Option<i64>]) -> Vec<u32> {
    let parts = runs(ring);
    parts
        .iter()
        .flat_map(|run| run.windows(NEED + 1))
        .filter_map(|w| period::measured(w, NEED))
        .collect()
}

/// The open's period decision: `ring` holds the sample positions of the
/// stream's first callbacks in order (`None` for a callback without one),
/// `waited` is the time since `start()`.
///
/// [`NEED`] agreeing deltas anywhere in the ring decide at once when they
/// give the expected period (a later miss never undoes that). Another
/// period, or none, refuses only once the ring is full or [`DECIDE_WITHIN`]
/// passed, since the expected period may still follow. So the verdict does
/// not depend on when the owner thread looks.
pub fn open_period(ring: &[Option<i64>], expected: u32, waited: Duration) -> OpenPeriod {
    let found = agreeing(ring);
    if found.contains(&expected) {
        OpenPeriod::Ok(expected)
    } else if ring.len() < RING && waited < DECIDE_WITHIN {
        OpenPeriod::Wait
    } else {
        OpenPeriod::Refuse(match found.first() {
            Some(&measured) => PeriodVerdict::Wrong { expected, measured },
            None => PeriodVerdict::Undecided,
        })
    }
}

/// When the stream's callback counter last moved: the stall rule's clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Watchdog {
    seen: u64,
    since: Instant,
}

impl Watchdog {
    /// A fresh stream at `now` (its counter starts at 0).
    pub fn new(now: Instant) -> Self {
        Self {
            seen: 0,
            since: now,
        }
    }

    /// Looks at the stream's callback count at `now`: true once it has not
    /// moved for [`reset::STALL`].
    pub fn stalled(&mut self, callbacks: u64, now: Instant) -> bool {
        if callbacks != self.seen {
            self.seen = callbacks;
            self.since = now;
        }
        reset::stalled(true, self.since, now)
    }
}

/// What asks the owner thread for a reopen in one tick; the log names it with
/// the budget's verdict (#9 2026-09-28).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Asked {
    /// The driver's reset request (`kAsioResetRequest`).
    pub reset: bool,
    /// The driver's buffer size change (`kAsioBufferSizeChange`).
    pub buffer_size: bool,
    /// The driver reported a new sample rate.
    pub rate: bool,
    /// HIL's forced reopen.
    pub forced: bool,
    /// No callback for [`reset::STALL`].
    pub stalled: bool,
}

impl Asked {
    /// Anything asks.
    pub fn any(self) -> bool {
        self.reset || self.buffer_size || self.rate || self.forced || self.stalled
    }
}

/// The reasons as a list, "nothing" when none.
impl fmt::Display for Asked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reasons: Vec<&str> = [
            (self.reset, "the driver's reset request"),
            (self.buffer_size, "the driver's buffer size change"),
            (self.rate, "the driver's sample-rate change"),
            (self.forced, "a forced reopen (HIL)"),
            (self.stalled, "a stall (no callback for 2 s)"),
        ]
        .into_iter()
        .filter_map(|(asked, why)| asked.then_some(why))
        .collect();
        if reasons.is_empty() {
            f.write_str("nothing")
        } else {
            f.write_str(&reasons.join(", "))
        }
    }
}

/// One tick's reopen question: the driver asked (a reset request, a buffer
/// size or a rate change), a reopen was forced (HIL) or the stream stalled.
/// `None` when nothing asks (the budget is untouched), else the budget's
/// verdict.
pub fn reset_step(asked: Asked, budget: &mut ResetBudget, now: Instant) -> Option<Verdict> {
    asked.any().then(|| budget.ask(now))
}

/// Whether the session-end handler releases the driver now: once the engine
/// asked the stream to stop, or after [`SESSION_END_WAIT`] at the latest.
pub fn session_end_release(stop_requested: bool, waited: Duration) -> bool {
    stop_requested || waited >= SESSION_END_WAIT
}

/// The SEH filter's next step (design §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SehStep {
    Wait,
    /// The driver is released: let the exception end the process.
    Continue,
    /// Still held after [`SEH_WAIT`]: the faulting thread sleeps for good
    /// (the stream is parked, the guard alarms).
    Park,
}

/// The SEH filter's step after waiting `waited` for the driver's release.
pub fn seh_step(released: bool, waited: Duration) -> SehStep {
    if released {
        SehStep::Continue
    } else if waited >= SEH_WAIT {
        SehStep::Park
    } else {
        SehStep::Wait
    }
}

/// What the owner thread does in a tick once a structured exception reached
/// the SEH filter (design §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SehRelease {
    /// No exception, or the stream already ended for good: nothing to do.
    Nothing,
    /// Release the driver for good: once `RELEASED` is set the filter lets
    /// the exception end the process.
    Release,
    /// The test hold (the parked-engine test, S6 design §10 test #2, #35;
    /// set only through the engine's fault-injection flag): the driver is
    /// kept as a driver that hangs in `dispose` keeps it (stopped, never
    /// disposed or released), so the filter's wait runs out and it parks the
    /// faulting thread.
    Hold,
}

/// The owner thread's answer to a structured exception (`seh`: the filter
/// ran) while the test hold is `hold` and the stream has `done` (ended for
/// good: a stop, the session end, an earlier release or hold).
pub fn seh_release(seh: bool, hold: bool, done: bool) -> SehRelease {
    if !seh || done {
        SehRelease::Nothing
    } else if hold {
        SehRelease::Hold
    } else {
        SehRelease::Release
    }
}

/// Whether a structured exception is a fault of the stream (the engine
/// saves, releases and exits 70). Under the test hold it parks the stream
/// instead (#35): the engine keeps running and reports `parked`, like a
/// stream a stuck callback parked (R6), until it ends (the test ends it with
/// an OS restart).
pub fn seh_faults(seh: bool, hold: bool) -> bool {
    seh && !hold
}

/// How a stream ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// Stopped, disposed and released; the driver module is free.
    Released,
    /// A callback stayed in the stream past the stop wait, or the owner
    /// thread gave no outcome: the stream is left allocated (never freed
    /// under a callback) and the guard alarms.
    Parked,
}

/// `AsioStream::stop`'s wait: the owner thread's outcome once it gave one;
/// `Parked` when the thread ended without one or gave none within
/// [`STOP_BOUND`]; `None` to keep waiting.
pub fn stop_step(
    outcome: Option<StopOutcome>,
    thread_ended: bool,
    waited: Duration,
) -> Option<StopOutcome> {
    match outcome {
        Some(o) => Some(o),
        None if thread_ended || waited >= STOP_BOUND => Some(StopOutcome::Parked),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// `n` positions from `start`, `step` apart.
    fn steps(start: i64, step: i64, n: usize) -> Vec<Option<i64>> {
        (0..n as i64).map(|i| Some(start + i * step)).collect()
    }

    /// A full ring of 32-sample steps whose deltas at `bad` are 64 (missed
    /// buffers).
    fn missed(bad: &[usize]) -> Vec<Option<i64>> {
        let mut at = 0;
        let mut ring = vec![Some(at)];
        for d in 0..RING - 1 {
            at += if bad.contains(&d) { 64 } else { 32 };
            ring.push(Some(at));
        }
        ring
    }

    #[test]
    fn the_constants_are_the_design_values() {
        assert_eq!((RING, NEED), (32, 8));
        assert_eq!(DECIDE_WITHIN, ms(1_000));
        assert_eq!(SESSION_END_WAIT, ms(3_000));
        assert_eq!(SEH_WAIT, ms(1_000));
        assert_eq!(STOP_BOUND, ms(5_000));
    }

    #[test]
    fn only_a_positive_buffer_is_a_frame_count() {
        assert_eq!(frames(32), Some(32));
        assert_eq!(frames(1), Some(1));
        assert_eq!(frames(i32::MAX), Some(2_147_483_647));
        assert_eq!(frames(0), None);
        assert_eq!(frames(-1), None);
        assert_eq!(frames(i32::MIN), None);
    }

    #[test]
    fn nine_steady_positions_decide_at_once() {
        let ring = steps(1_000, 32, 9);
        assert_eq!(open_period(&ring, 32, ms(0)), OpenPeriod::Ok(32));
        // The expected period is the caller's.
        assert_eq!(open_period(&steps(0, 64, 9), 64, ms(0)), OpenPeriod::Ok(64));
        // Eight positions give seven deltas: not yet.
        assert_eq!(
            open_period(&steps(1_000, 32, 8), 32, ms(0)),
            OpenPeriod::Wait
        );
        assert_eq!(open_period(&[], 32, ms(0)), OpenPeriod::Wait);
    }

    #[test]
    fn another_period_refuses_once_the_ring_is_full_or_a_second_passed() {
        let wrong = OpenPeriod::Refuse(PeriodVerdict::Wrong {
            expected: 32,
            measured: 64,
        });
        assert_eq!(open_period(&steps(0, 64, 32), 32, ms(0)), wrong);
        // Until then the expected period may still follow.
        assert_eq!(open_period(&steps(0, 64, 31), 32, ms(0)), OpenPeriod::Wait);
        assert_eq!(open_period(&steps(0, 64, 9), 32, ms(999)), OpenPeriod::Wait);
        assert_eq!(open_period(&steps(0, 64, 9), 32, ms(1_000)), wrong);
        // Eleven deltas of 64, then the expected period: the open is fine.
        let mut settles = steps(0, 64, 12);
        settles.extend(steps(736, 32, 20));
        assert_eq!(open_period(&settles[..12], 32, ms(0)), OpenPeriod::Wait);
        assert_eq!(open_period(&settles, 32, ms(0)), OpenPeriod::Ok(32));
        // Two other periods: the refusal names the first.
        let mut two = steps(0, 64, 9);
        two.extend(steps(528, 16, 23));
        assert_eq!(open_period(&two, 32, ms(0)), wrong);
    }

    #[test]
    fn two_bad_deltas_anywhere_in_a_full_ring_still_decide() {
        assert_eq!(missed(&[]).len(), 32);
        for a in 0..RING - 1 {
            for b in a..RING - 1 {
                assert_eq!(
                    open_period(&missed(&[a, b]), 32, ms(0)),
                    OpenPeriod::Ok(32),
                    "missed buffers at deltas {a} and {b}"
                );
            }
        }
        // One late position: the delta before it grows, the one after it
        // shrinks.
        for late in 1..RING - 1 {
            let mut ring = steps(0, 32, RING);
            ring[late] = ring[late].map(|p| p + 5);
            assert_eq!(
                open_period(&ring, 32, ms(0)),
                OpenPeriod::Ok(32),
                "late position {late}"
            );
        }
        // Three misses eight deltas apart leave seven agreeing ones in a row
        // at most; eight in a row decide.
        assert_eq!(
            open_period(&missed(&[7, 15, 23]), 32, ms(0)),
            OpenPeriod::Refuse(PeriodVerdict::Undecided)
        );
        assert_eq!(
            open_period(&missed(&[7, 16, 24]), 32, ms(0)),
            OpenPeriod::Ok(32)
        );
    }

    #[test]
    fn an_undecided_period_waits_until_the_ring_is_full_or_a_second_passed() {
        // Deltas alternate: never eight agreeing ones.
        let jitter =
            |n: i64| -> Vec<Option<i64>> { (0..n).map(|i| Some(i * 32 + (i % 2) * 5)).collect() };
        assert_eq!(open_period(&jitter(15), 32, ms(999)), OpenPeriod::Wait);
        assert_eq!(
            open_period(&jitter(15), 32, ms(1_000)),
            OpenPeriod::Refuse(PeriodVerdict::Undecided)
        );
        assert_eq!(
            open_period(&jitter(15), 32, ms(5_000)),
            OpenPeriod::Refuse(PeriodVerdict::Undecided)
        );
        assert_eq!(open_period(&jitter(31), 32, ms(0)), OpenPeriod::Wait);
        assert_eq!(
            open_period(&jitter(32), 32, ms(0)),
            OpenPeriod::Refuse(PeriodVerdict::Undecided)
        );
        // Too few positions after a second refuse too.
        assert_eq!(
            open_period(&steps(0, 32, 3), 32, ms(1_000)),
            OpenPeriod::Refuse(PeriodVerdict::Undecided)
        );
        // A decided period never waits for the clock.
        assert_eq!(
            open_period(&steps(0, 32, 16), 32, ms(1_000)),
            OpenPeriod::Ok(32)
        );
    }

    #[test]
    fn a_callback_without_a_position_splits_the_count() {
        // Nine good positions decide; a later gap does not undo that.
        let mut ring = steps(0, 32, 9);
        ring.push(None);
        ring.extend(steps(1_000, 32, 3));
        assert_eq!(open_period(&ring, 32, ms(0)), OpenPeriod::Ok(32));
        // Five and five around a gap never make nine in a row, even where
        // the positions line up across it.
        let mut ring = steps(0, 32, 5);
        ring.push(None);
        ring.extend(steps(160, 32, 5));
        assert_eq!(open_period(&ring, 32, ms(0)), OpenPeriod::Wait);
        // A negative position is no position either.
        let mut ring = steps(0, 32, 5);
        ring.push(Some(-32));
        ring.extend(steps(160, 32, 5));
        assert_eq!(open_period(&ring, 32, ms(0)), OpenPeriod::Wait);
        // Nine good positions after a gap decide.
        let mut ring = vec![Some(0), None];
        ring.extend(steps(64, 32, 9));
        assert_eq!(open_period(&ring, 32, ms(0)), OpenPeriod::Ok(32));
        let mut ring = steps(0, 64, 4);
        ring.push(None);
        ring.extend(steps(500, 32, 9));
        assert_eq!(open_period(&ring, 32, ms(0)), OpenPeriod::Ok(32));
        // A full ring with a gap every ninth callback: runs of eight
        // positions (seven deltas) never decide; every tenth: runs of nine do.
        let gaps = |every: i64| -> Vec<Option<i64>> {
            (0..32)
                .map(|i| (i % every != every - 1).then_some(i * 32))
                .collect()
        };
        assert_eq!(
            open_period(&gaps(9), 32, ms(0)),
            OpenPeriod::Refuse(PeriodVerdict::Undecided)
        );
        assert_eq!(open_period(&gaps(10), 32, ms(0)), OpenPeriod::Ok(32));
    }

    #[test]
    fn a_stall_is_two_seconds_without_a_moving_counter() {
        let t0 = Instant::now();
        let mut w = Watchdog::new(t0);
        assert!(!w.stalled(0, t0 + ms(1_999)));
        assert!(w.stalled(0, t0 + ms(2_000)));
        // The counter moves: the clock restarts there.
        assert!(!w.stalled(5, t0 + ms(2_500)));
        assert!(!w.stalled(5, t0 + ms(4_499)));
        assert!(w.stalled(5, t0 + ms(4_500)));
        // Any change counts, also a smaller count.
        assert!(!w.stalled(3, t0 + ms(5_000)));
        assert!(!w.stalled(3, t0 + ms(6_999)));
        assert!(w.stalled(3, t0 + ms(7_000)));
    }

    #[test]
    fn a_moving_counter_keeps_a_fresh_watchdog_quiet() {
        let t0 = Instant::now();
        let mut w = Watchdog::new(t0);
        assert!(!w.stalled(9, t0 + ms(1_500)));
        assert!(!w.stalled(9, t0 + ms(3_000)));
        assert!(w.stalled(9, t0 + ms(3_500)));
    }

    /// Each reason alone.
    fn each_reason() -> [Asked; 5] {
        let none = Asked::default();
        [
            Asked {
                reset: true,
                ..none
            },
            Asked {
                buffer_size: true,
                ..none
            },
            Asked { rate: true, ..none },
            Asked {
                forced: true,
                ..none
            },
            Asked {
                stalled: true,
                ..none
            },
        ]
    }

    /// Every reason at once.
    const EVERY: Asked = Asked {
        reset: true,
        buffer_size: true,
        rate: true,
        forced: true,
        stalled: true,
    };

    #[test]
    fn only_a_request_a_forced_reopen_or_a_stall_asks_the_budget() {
        let t0 = Instant::now();
        let mut b = ResetBudget::default();
        assert!(!Asked::default().any());
        assert_eq!(reset_step(Asked::default(), &mut b, t0), None);
        assert_eq!(b.used(), 0);
        for asked in each_reason() {
            assert!(asked.any(), "{asked:?}");
            let mut b = ResetBudget::default();
            assert_eq!(
                reset_step(asked, &mut b, t0),
                Some(Verdict::Reopen),
                "{asked:?}"
            );
            assert_eq!(b.used(), 1);
            // Within five minutes the next one is over budget.
            assert_eq!(
                reset_step(asked, &mut b, t0 + ms(1)),
                Some(Verdict::Fault),
                "{asked:?}"
            );
            assert_eq!(b.used(), 1);
            // Nothing asks: the budget is not asked either.
            assert_eq!(reset_step(Asked::default(), &mut b, t0 + ms(2)), None);
        }
        assert!(EVERY.any());
        let mut b = ResetBudget::default();
        assert_eq!(reset_step(EVERY, &mut b, t0), Some(Verdict::Reopen));
        assert_eq!(b.used(), 1);
    }

    /// Every reopen and an over-budget fault name their reasons in the log
    /// (#9 2026-09-28).
    #[test]
    fn the_reasons_for_a_reopen_read_as_a_list() {
        let texts: Vec<String> = each_reason().iter().map(Asked::to_string).collect();
        assert_eq!(
            texts,
            [
                "the driver's reset request",
                "the driver's buffer size change",
                "the driver's sample-rate change",
                "a forced reopen (HIL)",
                "a stall (no callback for 2 s)",
            ]
        );
        assert_eq!(
            EVERY.to_string(),
            "the driver's reset request, the driver's buffer size change, \
             the driver's sample-rate change, a forced reopen (HIL), \
             a stall (no callback for 2 s)"
        );
        assert_eq!(
            Asked {
                reset: true,
                stalled: true,
                ..Asked::default()
            }
            .to_string(),
            "the driver's reset request, a stall (no callback for 2 s)"
        );
        assert_eq!(Asked::default().to_string(), "nothing");
    }

    #[test]
    fn the_session_end_releases_on_the_stop_or_after_three_seconds() {
        assert!(session_end_release(true, ms(0)));
        assert!(!session_end_release(false, ms(0)));
        assert!(!session_end_release(false, ms(2_999)));
        assert!(session_end_release(false, ms(3_000)));
        assert!(session_end_release(false, ms(3_001)));
    }

    #[test]
    fn the_seh_filter_continues_once_released_and_parks_after_a_second() {
        assert_eq!(seh_step(true, ms(0)), SehStep::Continue);
        assert_eq!(seh_step(true, ms(5_000)), SehStep::Continue);
        assert_eq!(seh_step(false, ms(0)), SehStep::Wait);
        assert_eq!(seh_step(false, ms(999)), SehStep::Wait);
        assert_eq!(seh_step(false, ms(1_000)), SehStep::Park);
        assert_eq!(seh_step(false, ms(1_001)), SehStep::Park);
    }

    /// The parked-engine test (S6 design §10 test #2, #35): a structured
    /// exception under the test hold keeps the driver, so the SEH filter
    /// parks; without the hold the owner releases, as before.
    #[test]
    fn the_owner_releases_after_a_structured_exception_unless_the_test_hold_keeps_the_driver() {
        use SehRelease::{Hold, Nothing, Release};
        // No exception: nothing, whatever the hold and the stream.
        for (hold, done) in [(false, false), (true, false), (false, true), (true, true)] {
            assert_eq!(seh_release(false, hold, done), Nothing, "{hold} {done}");
        }
        assert_eq!(seh_release(true, false, false), Release);
        assert_eq!(seh_release(true, true, false), Hold);
        // A stream that already ended for good is left as it is.
        assert_eq!(seh_release(true, false, true), Nothing);
        assert_eq!(seh_release(true, true, true), Nothing);
    }

    /// A held structured exception parks the stream without a fault, so the
    /// engine keeps running and reports `parked` until it ends (#35); any
    /// other one is a fault, as before.
    #[test]
    fn a_structured_exception_is_a_fault_unless_the_test_hold_parks_it() {
        assert!(seh_faults(true, false));
        assert!(!seh_faults(true, true));
        assert!(!seh_faults(false, false));
        assert!(!seh_faults(false, true));
    }

    /// HIL v2 (S7, #10): a reopen's time, from the old stream's stop to the
    /// new one's measured period, in whole µs; never 0 (a reopen happened),
    /// and a time past u64 µs saturates.
    #[test]
    fn a_reopen_time_is_whole_microseconds_never_zero_and_saturates() {
        assert_eq!(reopen_us(Duration::from_micros(104_250)), 104_250);
        assert_eq!(reopen_us(Duration::from_nanos(104_250_999)), 104_250);
        assert_eq!(reopen_us(Duration::from_micros(2)), 2);
        assert_eq!(reopen_us(Duration::from_nanos(1_999)), 1);
        assert_eq!(reopen_us(Duration::from_nanos(999)), 1);
        assert_eq!(reopen_us(Duration::ZERO), 1);
        assert_eq!(reopen_us(Duration::from_micros(u64::MAX)), u64::MAX);
        assert_eq!(reopen_us(Duration::MAX), u64::MAX);
    }

    #[test]
    fn stop_takes_the_owners_outcome_or_parks() {
        use StopOutcome::{Parked, Released};
        assert_eq!(stop_step(Some(Released), false, ms(0)), Some(Released));
        assert_eq!(stop_step(Some(Released), true, ms(9_000)), Some(Released));
        assert_eq!(stop_step(Some(Parked), false, ms(0)), Some(Parked));
        assert_eq!(stop_step(None, false, ms(0)), None);
        assert_eq!(stop_step(None, false, ms(4_999)), None);
        assert_eq!(stop_step(None, false, ms(5_000)), Some(Parked));
        assert_eq!(stop_step(None, false, ms(5_001)), Some(Parked));
        assert_eq!(stop_step(None, true, ms(0)), Some(Parked));
    }
}
