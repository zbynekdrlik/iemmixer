//! Round-trip latency of the D5(b) Dante loopback (S6 design note §10 test 5,
//! iemmixer#9): the HIL test signal leaves on a spare card output, the owner
//! loops that output back to the matching spare card input in Dante, and this
//! probe measures the delay from the first emitted sample to the first sample
//! that returns above a threshold — in samples, then milliseconds.
//!
//! A threshold cannot tell an echo from a signal that is already on the
//! return, so a run is measured only when the return was quiet from
//! [`QUIET_BEFORE`] samples before the emit until [`MIN_ROUND_TRIP`] samples
//! after it; otherwise the run gives no measurement, never a false one
//! (#32 D3: a busy return read as a 16-sample echo).
//!
//! The probe runs on the RT thread and is RT-safe: it allocates nothing, only
//! compares samples and moves a few counters. It never runs in a live engine
//! (only under `--test-signal`, with the loopback return opened).

/// A return sample at or above this magnitude counts as the signal's onset.
/// The HIL test signal is at most −20 dBFS (`site::HIL_MAX_DBFS`); a loopback
/// at −60 dBFS is still far above the card's noise floor.
pub const ONSET: f64 = 0.001; // ≈ −60 dBFS

/// The smallest round-trip, in samples. The path is double-buffered (a
/// period in, a period out) plus the card's converters and Dante, so a real
/// loopback is far above this; a return that crosses the threshold within
/// this many samples of the emit is the emit leaking or an unrelated signal,
/// not an echo, and the run gives no measurement (iemmixer#9 review, #32).
pub const MIN_ROUND_TRIP: u64 = 16;

/// How long the return must have been quiet (below [`ONSET`]) right before
/// the emit for a measurement, in samples: 100 ms at 96 kHz, far longer than
/// any real loopback (the PC measured 129 samples; Dante's largest latency
/// setting is 5 ms). A return that sounded within it may still carry that
/// signal when the echo is due, so the run gives no measurement.
pub const QUIET_BEFORE: u64 = 9_600;

/// Measures the loopback round-trip. Fed the engine's continuous sample clock
/// so the emit and the arrival share one timeline: every block of every
/// return, in time order, from the stream's start (a time before it counts
/// as quiet: nothing was open to sound).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LatencyProbe {
    /// The sample index of the first emitted (non-silent) output sample.
    emitted_at: Option<u64>,
    /// The sample index of the echo's onset: the earliest return sample at or
    /// above [`ONSET`] from [`MIN_ROUND_TRIP`] after the emit on.
    arrived_at: Option<u64>,
    /// This run cannot be measured: the return sounded too close to the emit.
    spoiled: bool,
    /// The latest return sample at or above [`ONSET`]. What the return
    /// carried, not the run's: a reset keeps it.
    last_loud: Option<u64>,
}

impl LatencyProbe {
    pub fn new() -> Self {
        Self::default()
    }

    /// Clears the measurement for a new run (a new test signal). What the
    /// return carried stays: a signal just before the new emit spoils it.
    pub fn reset(&mut self) {
        self.emitted_at = None;
        self.arrived_at = None;
        self.spoiled = false;
    }

    /// Records the emit time once: `at` is the sample index of the first
    /// non-silent output sample of the test signal. Later calls are ignored,
    /// so the onset of the very first block is kept. A return that sounded
    /// within [`QUIET_BEFORE`] samples before it spoils the run.
    pub fn emitted(&mut self, at: u64) {
        if self.emitted_at.is_some() {
            return;
        }
        self.emitted_at = Some(at);
        if self
            .last_loud
            .is_some_and(|l| l.saturating_add(QUIET_BEFORE) >= at)
        {
            self.spoiled = true;
        }
    }

    /// Scans one block of one loopback return. `base` is the sample index of
    /// the block's first sample. The engine feeds every return of every
    /// block, in time order; the returns of one block may come in any order.
    ///
    /// Before the emit it only notes the return's latest onset. After it,
    /// the earliest sample at or above [`ONSET`] from [`MIN_ROUND_TRIP`] on,
    /// over every return, is the arrival; one earlier (the emit leaking or an
    /// unrelated signal already on the input) spoils the run, even after an
    /// arrival read on another return of the same block.
    pub fn feed(&mut self, ret: &[f64], base: u64) {
        for (i, &s) in ret.iter().enumerate() {
            if s.abs() >= ONSET {
                self.loud(base.saturating_add(i as u64));
            }
        }
    }

    /// A return sample at or above [`ONSET`] at `at`.
    fn loud(&mut self, at: u64) {
        self.last_loud = Some(self.last_loud.map_or(at, |l| l.max(at)));
        let Some(emit) = self.emitted_at else {
            return;
        };
        if self.spoiled {
            return;
        }
        if at < emit.saturating_add(MIN_ROUND_TRIP) {
            self.spoiled = true;
            self.arrived_at = None;
        } else {
            self.arrived_at = Some(self.arrived_at.map_or(at, |a| a.min(at)));
        }
    }

    /// The round-trip delay in samples, once both the emit and the arrival are
    /// known.
    pub fn samples(&self) -> Option<u64> {
        match (self.emitted_at, self.arrived_at) {
            (Some(e), Some(a)) if a >= e => Some(a - e),
            _ => None,
        }
    }

    /// The round-trip delay in milliseconds at `sample_rate`.
    pub fn ms(&self, sample_rate: u32) -> Option<f64> {
        let sr = f64::from(sample_rate);
        (sr > 0.0)
            .then(|| self.samples())
            .flatten()
            .map(|n| n as f64 * 1000.0 / sr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_probe_has_no_measurement() {
        let p = LatencyProbe::new();
        assert_eq!(p.samples(), None);
        assert_eq!(p.ms(96_000), None);
    }

    #[test]
    fn feed_before_emit_is_ignored() {
        let mut p = LatencyProbe::new();
        p.feed(&[1.0, 1.0], 0);
        assert_eq!(p.samples(), None);
        // Emit long after that return (farther than any quiet window before
        // an emit); a later return is measured from the emit.
        p.emitted(100_000);
        p.feed(&[0.0, 0.5], 100_048); // onset at index 1 → sample 100_049
        assert_eq!(p.samples(), Some(49));
    }

    #[test]
    fn the_round_trip_is_the_arrival_minus_the_emit() {
        let mut p = LatencyProbe::new();
        p.emitted(1_000);
        // Block starting at 1_000 is below onset (still travelling).
        p.feed(&[0.0; 32], 1_000);
        assert_eq!(p.samples(), None);
        // Block at 1_032: first sample above ONSET at index 5 → sample 1_037.
        let mut block = [0.0; 32];
        block[5] = 0.2;
        p.feed(&block, 1_032);
        assert_eq!(p.samples(), Some(37));
        // 37 samples at 96 kHz.
        let ms = p.ms(96_000).unwrap();
        assert!((ms - 37.0 * 1000.0 / 96_000.0).abs() < 1e-9, "{ms}");
    }

    #[test]
    fn the_arrival_is_recorded_only_once() {
        let mut p = LatencyProbe::new();
        p.emitted(0);
        let mut a = [0.0; 8];
        a[2] = 0.5;
        p.feed(&a, 20); // arrival at 22 (≥ MIN_ROUND_TRIP)
        p.feed(&[1.0; 8], 100); // later, stronger — must not move the arrival
        assert_eq!(p.samples(), Some(22));
    }

    #[test]
    fn a_return_within_the_minimum_round_trip_is_not_the_echo() {
        // A return that crosses the threshold too soon after the emit is the
        // emit leaking or an unrelated input, not an echo (iemmixer#9 review).
        let mut p = LatencyProbe::new();
        p.emitted(0);
        // Energy from sample 0 up to just below MIN_ROUND_TRIP is not the echo.
        let early = vec![0.5; (MIN_ROUND_TRIP - 1) as usize];
        p.feed(&early, 0);
        assert_eq!(p.samples(), None);
    }

    /// An echo `delay` samples after `emit` on an otherwise quiet return.
    fn echo(p: &mut LatencyProbe, emit: u64, delay: u64) {
        p.feed(&[0.0; 4], emit);
        p.feed(&[0.4; 4], emit + delay);
    }

    #[test]
    fn a_return_already_sounding_at_the_emit_is_no_measurement() {
        // #32 D3: an unrelated signal already on the return crossed the
        // threshold at emit + MIN_ROUND_TRIP and read as a 16-sample echo.
        let mut p = LatencyProbe::new();
        p.feed(&[0.5; 32], 0);
        p.emitted(32);
        p.feed(&[0.5; 64], 32);
        assert_eq!(p.samples(), None);
        // A later, louder return is no echo of this run either.
        p.feed(&[1.0; 8], 1_000);
        assert_eq!(p.samples(), None);
    }

    #[test]
    fn a_leak_within_the_minimum_round_trip_spoils_the_run() {
        // A return above the threshold before the echo can have come back
        // (the emit leaking, an unrelated input): the echo after it cannot be
        // told apart, so the run gives no measurement.
        let mut p = LatencyProbe::new();
        p.emitted(0);
        let mut leak = [0.0; 8];
        leak[3] = 0.2;
        p.feed(&leak, 0);
        echo(&mut p, 0, 129);
        assert_eq!(p.samples(), None);
        // The window's last sample spoils too; its end is the first echo.
        let mut p = LatencyProbe::new();
        p.emitted(0);
        p.feed(&[0.2], MIN_ROUND_TRIP - 1);
        echo(&mut p, 0, 129);
        assert_eq!(p.samples(), None);
        let mut p = LatencyProbe::new();
        p.emitted(0);
        p.feed(&[0.2], MIN_ROUND_TRIP);
        assert_eq!(p.samples(), Some(MIN_ROUND_TRIP));
    }

    #[test]
    fn the_return_must_be_quiet_for_the_window_before_the_emit() {
        // Loud QUIET_BEFORE samples before the emit: inside the window.
        let mut p = LatencyProbe::new();
        p.feed(&[0.3], 0);
        p.emitted(QUIET_BEFORE);
        echo(&mut p, QUIET_BEFORE, 129);
        assert_eq!(p.samples(), None);
        // One sample earlier is outside it: the echo is measured.
        let mut p = LatencyProbe::new();
        p.feed(&[0.3], 0);
        p.emitted(QUIET_BEFORE + 1);
        echo(&mut p, QUIET_BEFORE + 1, 129);
        assert_eq!(p.samples(), Some(129));
        // The latest loud sample counts, not the first.
        let mut p = LatencyProbe::new();
        p.feed(&[0.3, 0.0, 0.3], 0);
        p.emitted(QUIET_BEFORE + 1);
        echo(&mut p, QUIET_BEFORE + 1, 129);
        assert_eq!(p.samples(), None);
    }

    #[test]
    fn a_reset_keeps_what_the_return_carried() {
        // A new signal right after the last one's echo: the return is not
        // quiet before the new emit, so there is no clean measurement.
        let mut p = LatencyProbe::new();
        p.emitted(0);
        p.feed(&[0.5; 64], 129);
        assert_eq!(p.samples(), Some(129));
        p.reset();
        p.emitted(400);
        echo(&mut p, 400, 129);
        assert_eq!(p.samples(), None);
    }

    #[test]
    fn the_returns_of_one_block_are_judged_together() {
        // Each return is fed in turn for the same block. The earliest onset
        // is the arrival, whichever return is fed first.
        let mut p = LatencyProbe::new();
        p.emitted(0);
        let mut late = [0.0; 64];
        late[50] = 0.3;
        let mut early = [0.0; 64];
        early[30] = 0.3;
        p.feed(&late, 0);
        p.feed(&early, 0);
        assert_eq!(p.samples(), Some(30));
        // A leak on a later-fed return spoils an arrival read on an earlier one.
        let mut p = LatencyProbe::new();
        p.emitted(0);
        let mut leak = [0.0; 64];
        leak[5] = 0.3;
        p.feed(&late, 0);
        p.feed(&leak, 0);
        assert_eq!(p.samples(), None);
    }

    #[test]
    fn a_sample_just_below_onset_is_not_the_arrival() {
        let mut p = LatencyProbe::new();
        p.emitted(0);
        p.feed(&[ONSET * 0.9; 40], 0);
        assert_eq!(p.samples(), None);
        p.feed(&[ONSET; 4], 40); // exactly at the threshold counts
        assert_eq!(p.samples(), Some(40));
    }

    #[test]
    fn a_negative_going_return_still_triggers_on_magnitude() {
        let mut p = LatencyProbe::new();
        p.emitted(0);
        p.feed(&[-0.3, 0.0], 20); // |−0.3| ≥ ONSET at sample 20
        assert_eq!(p.samples(), Some(20));
    }

    #[test]
    fn reset_clears_a_measurement() {
        let mut p = LatencyProbe::new();
        p.emitted(0);
        p.feed(&[1.0], 20);
        assert_eq!(p.samples(), Some(20));
        p.reset();
        assert_eq!(p.samples(), None);
        // The next signal is measured from its own emit.
        p.emitted(100_000);
        p.feed(&[1.0], 100_030);
        assert_eq!(p.samples(), Some(30));
    }

    #[test]
    fn an_arrival_before_the_emit_is_no_measurement() {
        // The public API can only record an arrival after the emit, but the
        // `a >= e` guard must still refuse an inverted pair rather than
        // underflow. Constructed directly so `a < e`; `samples()` is None.
        let pair = |e, a| {
            let mut p = LatencyProbe::new();
            p.emitted_at = Some(e);
            p.arrived_at = Some(a);
            p
        };
        let p = pair(100, 50);
        assert_eq!(p.samples(), None);
        assert_eq!(p.ms(96_000), None);
        // The equal edge: a zero round-trip is a valid measurement.
        assert_eq!(pair(100, 100).samples(), Some(0));
    }

    #[test]
    fn ms_needs_a_positive_sample_rate() {
        let mut p = LatencyProbe::new();
        p.emitted(0);
        p.feed(&[1.0; 4], 96); // 96 samples
        assert_eq!(p.samples(), Some(96));
        assert_eq!(p.ms(0), None);
        assert_eq!(p.ms(96_000), Some(1.0)); // 96 samples at 96 kHz = 1 ms
    }
}
