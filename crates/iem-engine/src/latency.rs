//! Round-trip latency of the D5(b) Dante loopback (S6 design note §10 test 5,
//! iemmixer#9): the HIL test signal leaves on a spare card output, the owner
//! loops that output back to the matching spare card input in Dante, and this
//! probe measures the delay from the first emitted sample to the first sample
//! that returns above a threshold — in samples, then milliseconds.
//!
//! The probe runs on the RT thread and is RT-safe: it allocates nothing, only
//! compares samples and moves a few counters. It never runs in a live engine
//! (only under `--test-signal`, with the loopback return opened).

/// A return sample at or above this magnitude counts as the signal's onset.
/// The HIL test signal is at most −20 dBFS (`site::HIL_MAX_DBFS`); a loopback
/// at −60 dBFS is still far above the card's noise floor.
pub const ONSET: f64 = 0.001; // ≈ −60 dBFS

/// The smallest round-trip the probe accepts, in samples. The path is
/// double-buffered (a period in, a period out) plus the card's converters and
/// Dante, so a real loopback is far above this; a return that crosses the
/// threshold within this many samples of the emit is the emit leaking or an
/// unrelated signal, not an echo, and is ignored (iemmixer#9 review).
pub const MIN_ROUND_TRIP: u64 = 16;

/// Measures the loopback round-trip. Fed the engine's continuous sample clock
/// so the emit and the arrival share one timeline.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LatencyProbe {
    /// The sample index of the first emitted (non-silent) output sample.
    emitted_at: Option<u64>,
    /// The sample index of the first return sample above [`ONSET`].
    arrived_at: Option<u64>,
}

impl LatencyProbe {
    pub fn new() -> Self {
        Self::default()
    }

    /// Clears the measurement for a new run (a new test signal).
    pub fn reset(&mut self) {
        self.emitted_at = None;
        self.arrived_at = None;
    }

    /// Records the emit time once: `at` is the sample index of the first
    /// non-silent output sample of the test signal. Later calls are ignored,
    /// so the onset of the very first block is kept.
    pub fn emitted(&mut self, at: u64) {
        if self.emitted_at.is_none() {
            self.emitted_at = Some(at);
        }
    }

    /// Scans one block of the loopback return. `base` is the sample index of
    /// the block's first sample. Records the arrival once, at the first sample
    /// at or above [`ONSET`] whose delay from the emit is at least
    /// [`MIN_ROUND_TRIP`] — a return within that window is the emit leaking or
    /// an unrelated signal already on the input, not an echo (iemmixer#9
    /// review). Does nothing until the signal has been emitted.
    ///
    /// A clean measurement needs the return silent when the signal starts, so
    /// the engine runs one HIL test signal at a time (`start_test` resets the
    /// probe, and the HIL job serialises the signals).
    pub fn feed(&mut self, ret: &[f64], base: u64) {
        if self.arrived_at.is_some() {
            return;
        }
        let Some(emit) = self.emitted_at else {
            return;
        };
        let earliest = emit.saturating_add(MIN_ROUND_TRIP);
        for (i, &s) in ret.iter().enumerate() {
            if s.abs() >= ONSET {
                let at = base.saturating_add(i as u64);
                if at >= earliest {
                    self.arrived_at = Some(at);
                    return;
                }
            }
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
        // Now emit; a later return is measured from the emit.
        p.emitted(100);
        p.feed(&[0.0, 0.5], 148); // onset at index 1 → sample 149
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
    fn a_return_within_the_minimum_round_trip_is_ignored() {
        // A return that crosses the threshold too soon after the emit is the
        // emit leaking or an unrelated input, not an echo (iemmixer#9 review).
        let mut p = LatencyProbe::new();
        p.emitted(0);
        // Energy from sample 0 up to just below MIN_ROUND_TRIP is not the echo.
        let early = vec![0.5; (MIN_ROUND_TRIP - 1) as usize];
        p.feed(&early, 0);
        assert_eq!(p.samples(), None);
        // The first sample at or beyond the minimum is the arrival.
        p.feed(&[0.5; 4], MIN_ROUND_TRIP);
        assert_eq!(p.samples(), Some(MIN_ROUND_TRIP));
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
        assert_eq!(p, LatencyProbe::new());
        assert_eq!(p.samples(), None);
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
