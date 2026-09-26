//! Measured pan law (S1b `pan_law`, program spec A5 as amended on #5) and the
//! mono downmix (A10). The gains have the exact sine-taper direction
//! atan2(gR, gL) = (p + 1)·π/4; the magnitude sqrt(gL² + gR²) is tabulated at
//! |p| = 0, 0.05, …, 1 (even in p) and interpolated with Catmull-Rom. One law for
//! send pan (mode 0 and 3), track pan, stereo and dual-mono sources and mono media.

use core::f64::consts::{FRAC_PI_4, SQRT_2};

use crate::ramp::{GAIN_MS, MUTE_MS, Ramp, samples};

/// sqrt(gL² + gR²) at |p| = k/20, from the gains measured at p = k/20.
pub const MAGNITUDE: [f64; 21] = [
    SQRT_2,             // 0.00
    1.413442003770097,  // 0.05
    1.4111213638902695, // 0.10
    1.407233637679557,  // 0.15
    1.4017484365850923, // 0.20
    1.3946224041647768, // 0.25
    1.3857983687078865, // 0.30
    1.3752042016004387, // 0.35
    1.3627513381348282, // 0.40
    1.3483329027347901, // 0.45
    1.3318213620807011, // 0.50
    1.3130656059467758, // 0.55
    1.2918873247387341, // 0.60
    1.2680765120099933, // 0.65
    1.2413858657590862, // 0.70
    1.2115237885030046, // 0.75
    1.1781455848733056, // 0.80
    1.1408423148202314, // 0.85
    1.0991265624823106, // 0.90
    1.052414098036256,  // 0.95
    1.0,                // 1.00
];

/// Catmull-Rom ghost node past |p| = 1: 2·m(1) − m(0.95).
const GHOST: f64 = 0.947585901963744;

fn node(k: usize) -> f64 {
    MAGNITUDE.get(k).copied().unwrap_or(GHOST)
}

/// Interpolated magnitude for `a` = |p| in [0, 1].
fn magnitude(a: f64) -> f64 {
    let x = a * 20.0;
    let i = (x as usize).min(20);
    let t = x - i as f64;
    let (y0, y1, y2, y3) = (node(i.abs_diff(1)), node(i), node(i + 1), node(i + 2));
    y1 + 0.5
        * t
        * (y2 - y0 + t * (2.0 * y0 - 5.0 * y1 + 4.0 * y2 - y3 + t * (3.0 * (y1 - y2) + y3 - y0)))
}

/// Pan gains (gL, gR) for pan `p` in [-1, 1] (clamped; non-finite is centre).
pub fn gains(p: f64) -> (f64, f64) {
    let p = if p.is_finite() {
        p.clamp(-1.0, 1.0)
    } else {
        0.0
    };
    let m = magnitude(p.abs());
    (
        m * ((1.0 - p) * FRAC_PI_4).sin(),
        m * ((1.0 + p) * FRAC_PI_4).sin(),
    )
}

/// Gain pair of a send or track: v·(1 − m)·gains(p).
pub fn send_gains(vol: f64, muted: bool, pan: f64) -> (f64, f64) {
    if muted {
        return (0.0, 0.0);
    }
    let (l, r) = gains(pan);
    (vol * l, vol * r)
}

/// A mono destination (A10, S1b `mono_downmix_half_sum`): (gL·L + gR·R)/2 on
/// its channel 1, where (gL, gR) are the send's gains; channel 2 stays silent.
pub fn mono_downmix(l: f64, r: f64, gl: f64, gr: f64) -> f64 {
    (gl * l + gr * r) * 0.5
}

/// Smoothed gain pair (X15): v·gains(p) ramps over 10 ms, mute over 5 ms.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StereoGain {
    l: Ramp,
    r: Ramp,
    on: Ramp,
}

impl StereoGain {
    pub fn new(sample_rate: f64, vol: f64, muted: bool, pan: f64) -> Self {
        let (gl, gr) = send_gains(vol, false, pan);
        let len = samples(GAIN_MS, sample_rate);
        Self {
            l: Ramp::new(gl, len),
            r: Ramp::new(gr, len),
            on: Ramp::new(if muted { 0.0 } else { 1.0 }, samples(MUTE_MS, sample_rate)),
        }
    }

    pub fn set(&mut self, vol: f64, muted: bool, pan: f64) {
        let (gl, gr) = send_gains(vol, false, pan);
        self.l.set(gl);
        self.r.set(gr);
        self.on.set(if muted { 0.0 } else { 1.0 });
    }

    /// Gains for the next sample.
    pub fn tick(&mut self) -> (f64, f64) {
        let on = self.on.tick();
        (self.l.tick() * on, self.r.tick() * on)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centre_and_edges() {
        let (l, r) = gains(0.0);
        assert!((l - 1.0).abs() < 1e-15 && (r - 1.0).abs() < 1e-15);
        assert_eq!(gains(-1.0), (1.0, 0.0));
        assert_eq!(gains(1.0), (0.0, 1.0));
        assert_eq!(gains(3.0), (0.0, 1.0));
        assert_eq!(gains(f64::NAN), gains(0.0));
        assert_eq!(gains(f64::NEG_INFINITY), gains(0.0));
    }

    #[test]
    fn the_law_is_exactly_mirror_symmetric() {
        for k in 0..=200 {
            let p = f64::from(k) / 200.0;
            let (l, r) = gains(p);
            assert_eq!(gains(-p), (r, l), "p = {p}");
        }
    }

    #[test]
    fn direction_is_the_sine_taper_and_the_grid_is_exact() {
        for k in -20i32..=20 {
            let p = f64::from(k) / 20.0;
            let (l, r) = gains(p);
            let want = MAGNITUDE[k.unsigned_abs() as usize];
            assert!((l.hypot(r) - want).abs() < 1e-15, "p = {p}");
            if k.abs() < 20 {
                assert!(
                    (r.atan2(l) - (p + 1.0) * FRAC_PI_4).abs() < 1e-15,
                    "p = {p}"
                );
            }
        }
    }

    #[test]
    fn between_nodes_the_curve_is_smooth_and_monotone() {
        // p = 0.5 measured: gL = 0.5096659701381918, gR = 1.2304424973876633.
        let (l, r) = gains(0.5);
        assert!((l - 0.5096659701381918).abs() < 1e-15);
        assert!((r - 1.2304424973876633).abs() < 1e-15);
        let mut last = f64::INFINITY;
        for k in 0..=1000 {
            let m = magnitude(f64::from(k) / 1000.0);
            assert!(m <= last + 1e-15, "magnitude rises at {k}");
            last = m;
        }
        // Midway between 0.5 and 0.55 the cubic sits between its nodes, near the chord.
        let mid = magnitude(0.525);
        assert!(mid < MAGNITUDE[10] && mid > MAGNITUDE[11]);
        assert!((mid - (MAGNITUDE[10] + MAGNITUDE[11]) / 2.0).abs() < 2e-3);
    }

    #[test]
    fn send_gains_scale_and_mute() {
        let (l, r) = gains(-0.25);
        assert_eq!(send_gains(0.5, false, -0.25), (0.5 * l, 0.5 * r));
        assert_eq!(send_gains(0.5, true, -0.25), (0.0, 0.0));
    }

    #[test]
    fn mono_downmix_is_half_the_panned_sum() {
        assert_eq!(mono_downmix(0.5, 0.25, 1.0, 2.0), 0.5);
        assert_eq!(mono_downmix(1.0, 0.0, 0.5, 9.0), 0.25);
    }

    #[test]
    fn stereo_gain_ramps_gain_over_10_ms_and_mute_over_5_ms() {
        let sr = 1000.0; // 10 samples for gain, 5 for mute
        let mut g = StereoGain::new(sr, 1.0, false, -1.0);
        assert_eq!(g.tick(), (1.0, 0.0));
        g.set(0.0, false, -1.0);
        let first = g.tick();
        assert!((first.0 - 0.9).abs() < 1e-15 && first.1 == 0.0);
        for _ in 0..9 {
            g.tick();
        }
        assert_eq!(g.tick(), (0.0, 0.0));
        let mut m = StereoGain::new(sr, 1.0, false, 1.0);
        m.set(1.0, true, 1.0);
        let steps: Vec<f64> = (0..6).map(|_| m.tick().1).collect();
        assert!((steps[0] - 0.8).abs() < 1e-15);
        assert_eq!(&steps[4..], &[0.0, 0.0]);
        let muted = StereoGain::new(sr, 1.0, true, 0.0).tick();
        assert_eq!(muted, (0.0, 0.0));
    }
}
