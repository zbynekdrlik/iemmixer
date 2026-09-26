// MGA JS Limiter, ported to Rust for iemmixer.
// Original JSFX: Copyright (C) 2008 Michael Gruhn (fixtures/MGA_JSLimiterST).
// Port: Copyright (C) 2026 iemmixer contributors.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! Faithful port of the MGA JS Limiter (`loser/MGA_JSLimiterST`), the output
//! limiter of every member bus (program spec A13, X14; S2 design note §3.4).
//!
//! - [`Mga`] is the JSFX itself with its four sliders: two staggered peak-hold
//!   windows of `sr/128` samples, the asymmetric stereo link, instant attack,
//!   exponential release, zero lookahead and the GR meter. It equals REAPER's
//!   output on the S1b goldens.
//! - [`Limiter`] is iemmixer's use of it: threshold = ceiling = the limit
//!   (−6…0 dB), release 50 ms, link 75 %; enabling is instant, disabling
//!   crossfades over 10 ms; the limiter-active sample count (X14).
//!
//! f64 throughout; no allocation, lock or panic on the process path (I7).

#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic
    )
)]

/// Release the product uses (slider 2, ms).
pub const RELEASE_MS: f64 = 50.0;
/// Stereo link the product uses (slider 3, %).
pub const LINK_PCT: f64 = 75.0;
/// The product's limit range (dB).
pub const LIMIT_MIN_DB: f64 = -6.0;
pub const LIMIT_MAX_DB: f64 = 0.0;
/// X14: a sample is limiter-active while the GR meter is below −1 dB.
pub const ACTIVE_BELOW_DB: f64 = -1.0;
/// Disabling crossfades over this long (enabling is instant).
pub const DISABLE_MS: f64 = 10.0;
/// Envelopes below this are flushed to zero (denormals; `env ≤ thresh` there).
const DENORMAL: f64 = 1e-30;

/// The four JSFX sliders.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sliders {
    pub threshold_db: f64,
    pub release_ms: f64,
    pub link_pct: f64,
    pub ceiling_db: f64,
}

/// One channel's detector (JSFX r1Timer, r2Timer, max1Block, max2Block, env).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Channel {
    t1: f64,
    t2: f64,
    m1: f64,
    m2: f64,
    env: f64,
}

impl Channel {
    fn new(hold: f64) -> Self {
        Self {
            t1: 0.0,
            t2: hold / 2.0,
            m1: 0.0,
            m2: 0.0,
            env: 0.0,
        }
    }

    /// The held peak of |x| over two windows staggered by half a window (envT).
    fn held_peak(&mut self, x: f64, hold: f64) -> f64 {
        let a = x.abs();
        self.t1 += 1.0;
        if self.t1 > hold {
            self.t1 = 0.0;
            self.m1 = 0.0;
        }
        self.m1 = self.m1.max(a);
        self.t2 += 1.0;
        if self.t2 > hold {
            self.t2 = 0.0;
            self.m2 = 0.0;
        }
        self.m2 = self.m2.max(a);
        self.m1.max(self.m2)
    }

    /// Instant attack, release towards the held peak; returns the GR (g_meter).
    fn gain(&mut self, peak: f64, r: f64, thresh: f64) -> f64 {
        self.env = if self.env < peak {
            peak
        } else {
            peak + r * (self.env - peak)
        };
        let g = if self.env > thresh {
            thresh / self.env
        } else {
            1.0
        };
        if self.env < DENORMAL {
            self.env = 0.0;
        }
        g
    }
}

/// The JSFX (`@init`, `@slider`, `@sample`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mga {
    hold: f64,
    thresh: f64,
    volume: f64,
    r: f64,
    link: f64,
    left: Channel,
    right: Channel,
    gr_meter: f64,
    gr_decay: f64,
}

impl Mga {
    pub fn new(sample_rate: f64, sliders: Sliders) -> Self {
        let hold = sample_rate / 128.0;
        let mut m = Self {
            hold,
            thresh: 1.0,
            volume: 1.0,
            r: 0.0,
            link: 0.0,
            left: Channel::new(hold),
            right: Channel::new(hold),
            gr_meter: 1.0,
            gr_decay: (1.0 / sample_rate).exp(),
        };
        m.set_sliders(sample_rate, sliders);
        m
    }

    /// `@slider`.
    pub fn set_sliders(&mut self, sample_rate: f64, s: Sliders) {
        self.thresh = 10f64.powf(s.threshold_db / 20.0);
        let ceiling = 10f64.powf(s.ceiling_db / 20.0);
        self.volume = ceiling / self.thresh;
        let release = s.release_ms / 1000.0;
        self.r = (-3.0 / (sample_rate * release.max(0.05))).exp();
        self.link = (s.link_pct * 0.01).sqrt();
    }

    /// `@sample` for one stereo frame.
    pub fn tick(&mut self, xl: f64, xr: f64) -> (f64, f64) {
        let pl = self.left.held_peak(xl, self.hold);
        let pr = self.right.held_peak(xr, self.hold);
        // The link reads the other channel's previous envelope for L and the new
        // L envelope for R (JSFX order).
        self.left.env = self.left.env.max(self.right.env * self.link);
        self.right.env = (self.left.env * self.link).max(self.right.env);
        let gl = self.left.gain(pl, self.r, self.thresh);
        let gr = self.right.gain(pr, self.r, self.thresh);
        let g = gl.min(gr);
        if g < self.gr_meter {
            self.gr_meter = g;
        } else {
            self.gr_meter = (self.gr_meter * self.gr_decay).min(1.0);
        }
        (xl * (gl * self.volume), xr * (gr * self.volume))
    }

    /// The GR meter (linear, ≤ 1), as the JSFX reports it in `@block`.
    pub const fn gr_meter(&self) -> f64 {
        self.gr_meter
    }

    /// `@init` again (a node reset, X1); the sliders stay.
    pub fn reset(&mut self) {
        self.left = Channel::new(self.hold);
        self.right = Channel::new(self.hold);
        self.gr_meter = 1.0;
    }
}

/// iemmixer's output limiter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limiter {
    mga: Mga,
    sample_rate: f64,
    limit_db: f64,
    enabled: bool,
    wet: f64,
    fade_step: f64,
    active: u64,
    active_below: f64,
}

fn product_sliders(limit_db: f64) -> Sliders {
    Sliders {
        threshold_db: limit_db,
        release_ms: RELEASE_MS,
        link_pct: LINK_PCT,
        ceiling_db: limit_db,
    }
}

fn clamp_limit(db: f64) -> f64 {
    if db.is_finite() {
        db.clamp(LIMIT_MIN_DB, LIMIT_MAX_DB)
    } else {
        LIMIT_MIN_DB
    }
}

impl Limiter {
    /// An enabled limiter at `limit_db` (clamped to −6…0 dB; non-finite is −6).
    pub fn new(sample_rate: f64, limit_db: f64) -> Self {
        let limit_db = clamp_limit(limit_db);
        let fade = (DISABLE_MS * sample_rate / 1000.0).round().max(1.0);
        Self {
            mga: Mga::new(sample_rate, product_sliders(limit_db)),
            sample_rate,
            limit_db,
            enabled: true,
            wet: 1.0,
            fade_step: 1.0 / fade,
            active: 0,
            active_below: 10f64.powf(ACTIVE_BELOW_DB / 20.0),
        }
    }

    /// New limit; applies at the next sample, like the JSFX slider.
    pub fn set_limit_db(&mut self, db: f64) {
        self.limit_db = clamp_limit(db);
        self.mga
            .set_sliders(self.sample_rate, product_sliders(self.limit_db));
    }

    pub const fn limit_db(&self) -> f64 {
        self.limit_db
    }

    /// Enabling is instant (hearing protection); disabling fades out over 10 ms.
    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
        if on {
            self.wet = 1.0;
        }
    }

    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// Processes a stereo block in place (length: the shorter channel). The
    /// detector always runs, so re-enabling finds a current envelope.
    pub fn process(&mut self, left: &mut [f64], right: &mut [f64]) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (yl, yr) = self.mga.tick(*l, *r);
            if self.enabled {
                if self.mga.gr_meter() < self.active_below {
                    self.active += 1;
                }
            } else if self.wet > 0.0 {
                self.wet = (self.wet - self.fade_step).max(0.0);
            }
            if self.wet == 1.0 {
                (*l, *r) = (yl, yr);
            } else {
                *l += self.wet * (yl - *l);
                *r += self.wet * (yr - *r);
            }
        }
    }

    /// Gain reduction in dB as REAPER showed it (`@block`), floored at −150 dB
    /// (the JSFX floors only an exact 0); 0 while disabled.
    pub fn gr_db(&self) -> f64 {
        let g = self.mga.gr_meter();
        if !self.enabled {
            0.0
        } else if g > 0.0 {
            (20.0 * g.log10()).max(-150.0)
        } else {
            -150.0
        }
    }

    /// X14: samples with the GR meter below −1 dB while enabled.
    pub const fn active_samples(&self) -> u64 {
        self.active
    }

    /// X14: `active_samples` in seconds.
    pub fn active_seconds(&self) -> f64 {
        self.active as f64 / self.sample_rate
    }

    pub fn reset_active(&mut self) {
        self.active = 0;
    }

    /// Node reset after a sanitiser trip (X1): the detector restarts as at `@init`.
    pub fn reset(&mut self) {
        self.mga.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 96_000.0;

    fn run(l: &mut Limiter, left: &mut [f64], right: &mut [f64]) {
        l.process(left, right);
    }

    #[test]
    fn sliders_follow_the_jsfx_formulas() {
        let m = Mga::new(SR, product_sliders(-6.0));
        assert_eq!(m.hold, 750.0);
        assert_eq!(m.thresh, 10f64.powf(-0.3));
        assert_eq!(m.volume, 1.0);
        assert_eq!(m.r, (-3.0 / (SR * 0.05)).exp());
        assert_eq!(m.link, 0.75f64.sqrt());
        assert_eq!(m.gr_decay, (1.0 / SR).exp());
        assert_eq!((m.left.t2, m.right.t2, m.left.t1), (375.0, 375.0, 0.0));
        // Release has a 50 ms floor; threshold below ceiling is make-up gain.
        let fast = Mga::new(
            SR,
            Sliders {
                threshold_db: -12.0,
                release_ms: 10.0,
                link_pct: 0.0,
                ceiling_db: -6.0,
            },
        );
        assert_eq!(fast.r, m.r);
        assert!((fast.volume - 10f64.powf(0.3)).abs() < 1e-15);
        assert_eq!(fast.link, 0.0);
        let slow = Mga::new(
            44_100.0,
            Sliders {
                threshold_db: 0.0,
                release_ms: 200.0,
                link_pct: 100.0,
                ceiling_db: 0.0,
            },
        );
        assert_eq!(slow.hold, 344.53125);
        assert_eq!(slow.r, (-3.0 / (44_100.0 * 0.2)).exp());
        assert_eq!(slow.link, 1.0);
    }

    #[test]
    fn the_hold_windows_reset_after_hold_plus_one_samples() {
        let mut c = Channel::new(4.0);
        assert_eq!(c.held_peak(0.5, 4.0), 0.5); // t1 = 1, t2 = 3
        assert_eq!(c.held_peak(0.0, 4.0), 0.5); // t2 = 4
        assert_eq!(c.held_peak(0.0, 4.0), 0.5); // t2 > 4: window 2 restarts
        assert_eq!((c.m2, c.t2), (0.0, 0.0));
        assert_eq!(c.held_peak(0.0, 4.0), 0.5); // t1 = 4
        assert_eq!(c.held_peak(0.25, 4.0), 0.25); // t1 > 4: window 1 restarts
        assert_eq!((c.m1, c.t1), (0.25, 0.0));
    }

    #[test]
    fn attack_is_instant_and_release_is_exponential() {
        let mut c = Channel::new(4.0);
        assert_eq!(c.gain(2.0, 0.5, 1.0), 0.5);
        assert_eq!(c.env, 2.0);
        assert_eq!(c.gain(0.0, 0.5, 1.0), 1.0); // env 1.0 is not above thresh
        assert_eq!(c.env, 1.0);
        assert_eq!(c.gain(0.0, 0.5, 0.25), 0.5); // env 0.5
        c.env = 2e-30;
        c.gain(0.0, 0.25, 1.0);
        assert_eq!(c.env, 0.0);
    }

    #[test]
    fn the_output_never_exceeds_the_limit_and_gr_recovers() {
        for db in [-6.0, -3.0, 0.0] {
            let mut l = Limiter::new(SR, db);
            let ceiling = 10f64.powf(db / 20.0);
            let mut left: Vec<f64> = (0..4800).map(|i| 3.0 * ((i as f64) * 0.05).sin()).collect();
            let mut right = vec![0.0; 4800];
            run(&mut l, &mut left, &mut right);
            assert!(left.iter().all(|x| x.abs() <= ceiling * (1.0 + 1e-15)));
            assert!(left.iter().any(|x| x.abs() > 0.99 * ceiling));
            assert!(l.gr_db() < -9.0, "{}", l.gr_db());
            // Once the hold has passed, the GR meter recovers at exp(1/sr) per
            // sample (+8.7 dB/s): +0.8686 dB over 0.1 s.
            let (mut sl, mut sr) = (vec![0.0; 2000], vec![0.0; 2000]);
            run(&mut l, &mut sl, &mut sr);
            let before = l.gr_db();
            let (mut sl, mut sr) = (vec![0.0; 9600], vec![0.0; 9600]);
            run(&mut l, &mut sl, &mut sr);
            let rise = l.gr_db() - before;
            assert!((rise - 20.0 * 0.1 / 10f64.ln()).abs() < 1e-9, "{rise}");
        }
    }

    #[test]
    fn link_pulls_the_quiet_channel_down() {
        let mut l = Limiter::new(SR, -6.0);
        let mut left = vec![1.0; 100];
        let mut right = vec![0.1; 100];
        run(&mut l, &mut left, &mut right);
        // L env 1.0 → gain 0.501. R env is lifted to 0.866 (75 % link) and then
        // released one step towards its own peak 0.1 every sample.
        let g = 0.5011872336272722;
        assert!((left[99] - g).abs() < 1e-15);
        let env = 0.1 + (-3.0 / (SR * 0.05)).exp() * (0.75f64.sqrt() - 0.1);
        assert!((right[99] - 0.1 * (g / env)).abs() < 1e-15, "{}", right[99]);
    }

    #[test]
    fn activity_counts_samples_below_minus_1_db_while_enabled() {
        let mut l = Limiter::new(SR, -6.0);
        let mut left = vec![0.0; 1000];
        left[10] = 2.0;
        let mut right = vec![0.0; 1000];
        run(&mut l, &mut left, &mut right);
        // GR -12 dB at sample 10 recovers +8.7 dB/s: below -1 dB for the rest of the block.
        assert_eq!(l.active_samples(), 990);
        assert_eq!(l.active_seconds(), 990.0 / SR);
        l.reset_active();
        assert_eq!(l.active_samples(), 0);
        l.set_enabled(false);
        assert!(!l.enabled());
        let (mut a, mut b) = (vec![2.0; 50], vec![2.0; 50]);
        run(&mut l, &mut a, &mut b);
        assert_eq!(l.active_samples(), 0);
        assert_eq!(l.gr_db(), 0.0);
    }

    #[test]
    fn disabling_fades_out_over_10_ms_and_enabling_is_instant() {
        let mut l = Limiter::new(1000.0, -6.0); // 10 samples
        l.set_enabled(false);
        let mut left = vec![1.0; 12];
        let mut right = vec![1.0; 12];
        run(&mut l, &mut left, &mut right);
        let g = 0.5011872336272722;
        assert!(
            (left[0] - (1.0 + 0.9 * (g - 1.0))).abs() < 1e-12,
            "{}",
            left[0]
        );
        assert!(
            (left[8] - (1.0 + 0.1 * (g - 1.0))).abs() < 1e-12,
            "{}",
            left[8]
        );
        assert_eq!(&left[9..], &[1.0, 1.0, 1.0]);
        l.set_enabled(true);
        let (mut a, mut b) = ([1.0], [1.0]);
        run(&mut l, &mut a, &mut b);
        assert!((a[0] - g).abs() < 1e-15);
    }

    #[test]
    fn limits_are_clamped_and_gr_reads_like_reaper() {
        let mut l = Limiter::new(SR, -9.0);
        assert_eq!(l.limit_db(), -6.0);
        l.set_limit_db(3.0);
        assert_eq!(l.limit_db(), 0.0);
        l.set_limit_db(f64::NAN);
        assert_eq!(l.limit_db(), -6.0);
        l.set_limit_db(-3.0);
        assert_eq!(l.limit_db(), -3.0);
        assert_eq!(Limiter::new(SR, f64::INFINITY).limit_db(), -6.0);
        assert_eq!(l.gr_db(), 0.0);
        let (mut a, mut b) = ([4.0], [4.0]);
        run(&mut l, &mut a, &mut b);
        assert!((a[0] - 10f64.powf(-0.15)).abs() < 1e-15);
        assert!((l.gr_db() - (-3.0 - 20.0 * 4f64.log10())).abs() < 1e-12);
        l.mga.gr_meter = 0.0;
        assert_eq!(l.gr_db(), -150.0);
        l.mga.gr_meter = 1e-9;
        assert_eq!(l.gr_db(), -150.0);
        l.mga.gr_meter = 1e-7;
        assert!((l.gr_db() + 140.0).abs() < 1e-9);
    }

    #[test]
    fn reset_restarts_the_detector() {
        let mut l = Limiter::new(SR, -6.0);
        let (mut a, mut b) = (vec![3.0; 64], vec![3.0; 64]);
        run(&mut l, &mut a, &mut b);
        l.reset();
        assert_eq!(l.mga, Mga::new(SR, product_sliders(-6.0)));
        assert_eq!(l.active_samples(), 64);
    }

    #[test]
    fn the_shorter_channel_sets_the_length() {
        let mut l = Limiter::new(SR, -6.0);
        let (mut a, mut b) = (vec![2.0; 4], vec![2.0; 2]);
        run(&mut l, &mut a, &mut b);
        assert_eq!(&a[2..], &[2.0, 2.0]);
        assert!(a[1] < 0.6 && b[1] < 0.6);
    }
}
