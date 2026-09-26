//! Linear parameter ramps (X15). A change moves over a fixed number of
//! samples and the last step lands exactly on the target, so the output depends
//! only on sample indices, never on block boundaries, and steady state is exact.

/// Send and fader gain ramp (X15).
pub const GAIN_MS: f64 = 10.0;
/// Pan ramp (X15).
pub const PAN_MS: f64 = 10.0;
/// Mute ramp (X15).
pub const MUTE_MS: f64 = 5.0;
/// EQ parameter ramp (X15).
pub const EQ_MS: f64 = 20.0;

/// Samples in `ms` milliseconds at `sample_rate`, rounded, at least 1.
pub fn samples(ms: f64, sample_rate: f64) -> u32 {
    // `as` saturates and maps NaN to 0.
    ((ms * sample_rate / 1000.0).round() as u32).max(1)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ramp {
    value: f64,
    target: f64,
    step: f64,
    left: u32,
    len: u32,
}

impl Ramp {
    /// A ramp resting at `value`; each change takes `len` samples (at least 1).
    pub const fn new(value: f64, len: u32) -> Self {
        Self {
            value,
            target: value,
            step: 0.0,
            left: 0,
            len: if len == 0 { 1 } else { len },
        }
    }

    pub const fn value(&self) -> f64 {
        self.value
    }

    pub const fn target(&self) -> f64 {
        self.target
    }

    pub const fn is_moving(&self) -> bool {
        self.left > 0
    }

    /// Move towards `target` from the current value (restarts the full length).
    pub fn set(&mut self, target: f64) {
        if target == self.target {
            return;
        }
        self.target = target;
        self.left = self.len;
        self.step = (target - self.value) / f64::from(self.len);
    }

    /// Rest at `value` at once.
    pub fn jump(&mut self, value: f64) {
        *self = Self::new(value, self.len);
    }

    /// Advance one sample; returns the value for this sample.
    pub fn tick(&mut self) -> f64 {
        if self.left > 0 {
            self.left -= 1;
            self.value = if self.left == 0 {
                self.target
            } else {
                self.value + self.step
            };
        }
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_follow_x15_at_the_engine_rate() {
        assert_eq!(samples(GAIN_MS, 96_000.0), 960);
        assert_eq!(samples(PAN_MS, 48_000.0), 480);
        assert_eq!(samples(MUTE_MS, 96_000.0), 480);
        assert_eq!(samples(EQ_MS, 44_100.0), 882);
        assert_eq!(samples(0.01, 44_100.0), 1);
        assert_eq!(samples(1.0, 1_500.0), 2); // 1.5 rounds up
        assert_eq!(samples(f64::NAN, 96_000.0), 1);
    }

    #[test]
    fn a_ramp_moves_linearly_and_lands_on_the_target() {
        let mut r = Ramp::new(1.0, 4);
        assert!(!r.is_moving());
        assert_eq!(r.tick(), 1.0);
        r.set(3.0);
        assert!(r.is_moving());
        assert_eq!(r.target(), 3.0);
        let got: Vec<f64> = (0..6).map(|_| r.tick()).collect();
        assert_eq!(got, vec![1.5, 2.0, 2.5, 3.0, 3.0, 3.0]);
        assert!(!r.is_moving());
        assert_eq!(r.value(), 3.0);
    }

    #[test]
    fn a_new_target_restarts_from_the_current_value_and_the_same_target_does_not() {
        let mut r = Ramp::new(0.0, 2);
        r.set(1.0);
        assert_eq!(r.tick(), 0.5);
        r.set(1.0);
        assert_eq!(r.tick(), 1.0);
        r.set(-1.0);
        assert_eq!(r.tick(), 0.0);
        assert_eq!(r.tick(), -1.0);
        r.jump(0.25);
        assert!(!r.is_moving());
        assert_eq!((r.value(), r.target(), r.tick()), (0.25, 0.25, 0.25));
    }

    #[test]
    fn a_zero_length_is_one_sample() {
        let mut r = Ramp::new(0.0, 0);
        r.set(2.0);
        assert_eq!(r.tick(), 2.0);
        assert!(!r.is_moving());
    }
}
