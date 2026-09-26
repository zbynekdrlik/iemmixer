//! Peak meters (F9): the sample peak of each channel since the last read.
//! Hold and decay stay in the UI.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PeakMeter<const CH: usize> {
    peak: [f64; CH],
}

impl<const CH: usize> Default for PeakMeter<CH> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const CH: usize> PeakMeter<CH> {
    pub const fn new() -> Self {
        Self { peak: [0.0; CH] }
    }

    /// Takes |x| of every sample into the running peaks (NaN is ignored).
    pub fn observe(&mut self, chans: [&[f64]; CH]) {
        for (peak, ch) in self.peak.iter_mut().zip(chans) {
            for x in ch {
                let a = x.abs();
                if a > *peak {
                    *peak = a;
                }
            }
        }
    }

    /// The peaks since the last call; the meter starts again from zero.
    pub fn take(&mut self) -> [f64; CH] {
        core::mem::replace(&mut self.peak, [0.0; CH])
    }
}

/// dB of a linear value, floored at −150 dB (the JSFX GR readout's floor).
pub fn to_db(x: f64) -> f64 {
    if x > 0.0 {
        (20.0 * x.log10()).max(-150.0)
    } else {
        -150.0
    }
}

/// Seconds for a sample count (X14 active-seconds).
pub fn seconds(samples: u64, sample_rate: f64) -> f64 {
    samples as f64 / sample_rate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peaks_accumulate_per_channel_until_taken() {
        let mut m = PeakMeter::<2>::default();
        m.observe([&[0.1, -0.5], &[0.25]]);
        m.observe([&[0.3], &[-0.2, f64::NAN]]);
        assert_eq!(m.take(), [0.5, 0.25]);
        assert_eq!(m.take(), [0.0, 0.0]);
        let mut mono = PeakMeter::<1>::new();
        mono.observe([&[-1.5]]);
        assert_eq!(mono.take(), [1.5]);
    }

    #[test]
    fn db_and_seconds() {
        assert_eq!(to_db(1.0), 0.0);
        assert!((to_db(0.5) + 6.020599913279624).abs() < 1e-12);
        assert_eq!(to_db(1e-9), -150.0);
        assert_eq!(to_db(0.0), -150.0);
        assert_eq!(to_db(-1.0), -150.0);
        assert!((to_db(10.0) - 20.0).abs() < 1e-12);
        assert_eq!(seconds(48_000, 96_000.0), 0.5);
    }
}
