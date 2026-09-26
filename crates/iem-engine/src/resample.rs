//! 96 ↔ 48 kHz for the listen taps and talkback (X4; design note §3.4): a
//! 79-tap Kaiser-windowed half-band FIR (β = 10.06), linear phase, each
//! polyphase branch normalised to unity. Measured in numpy: ±1e-4 dB up to
//! 20 kHz, ≥ 99.6 dB rejection from 28 kHz (aliases land above 20 kHz).
//! Runs on the media thread, never on the RT thread.

use core::f64::consts::PI;

pub const TAPS: usize = 79;
pub const BETA: f64 = 10.06;
const CENTER: usize = TAPS / 2;

/// The modified Bessel function I0 by its power series.
fn bessel_i0(x: f64) -> f64 {
    let q = x * x / 4.0;
    let (mut sum, mut term) = (1.0, 1.0);
    for k in 1..100u32 {
        term *= q / f64::from(k * k);
        sum += term;
        if term < 1e-17 * sum {
            break;
        }
    }
    sum
}

/// The filter: symmetric, taps at even non-zero offsets exactly 0, the centre
/// 0.5 and the odd taps summing to 0.5.
pub fn half_band() -> [f64; TAPS] {
    let mut h = [0.0; TAPS];
    let norm = bessel_i0(BETA);
    let half = CENTER as f64;
    for (n, v) in h.iter_mut().enumerate() {
        let k = n as f64 - half;
        let off = n.abs_diff(CENTER);
        *v = if off == 0 {
            0.5
        } else if off % 2 == 0 {
            0.0
        } else {
            let r = k / half;
            let w = bessel_i0(BETA * (1.0 - r * r).max(0.0).sqrt()) / norm;
            (PI * k / 2.0).sin() / (PI * k) * w
        };
    }
    let odd: f64 = h
        .iter()
        .enumerate()
        .filter(|(n, _)| n.abs_diff(CENTER) % 2 == 1)
        .map(|(_, v)| v)
        .sum();
    for (n, v) in h.iter_mut().enumerate() {
        if n.abs_diff(CENTER) % 2 == 1 {
            *v *= 0.5 / odd;
        }
    }
    h
}

fn nonzero() -> Vec<(usize, f64)> {
    half_band()
        .into_iter()
        .enumerate()
        .filter(|(_, v)| *v != 0.0)
        .collect()
}

/// A delay line with the newest sample at `pos`: `x[n − j]` is at `pos + j`.
#[derive(Debug, Clone)]
struct Line {
    hist: Vec<f64>,
    pos: usize,
}

impl Line {
    fn new() -> Self {
        Self {
            hist: vec![0.0; TAPS],
            pos: 0,
        }
    }

    fn push(&mut self, x: f64) {
        self.pos = (self.pos + TAPS - 1) % TAPS;
        if let Some(v) = self.hist.get_mut(self.pos) {
            *v = x;
        }
    }

    fn dot(&self, taps: &[(usize, f64)]) -> f64 {
        taps.iter()
            .map(|(j, h)| h * self.hist.get((self.pos + j) % TAPS).copied().unwrap_or(0.0))
            .sum()
    }
}

/// Stereo 2:1 decimator: the first input and every second after it yield an
/// output.
#[derive(Debug, Clone)]
pub struct Decimator2 {
    taps: Vec<(usize, f64)>,
    lines: [Line; 2],
    skip: bool,
}

impl Default for Decimator2 {
    fn default() -> Self {
        Self::new()
    }
}

impl Decimator2 {
    pub fn new() -> Self {
        Self {
            taps: nonzero(),
            lines: [Line::new(), Line::new()],
            skip: false,
        }
    }

    pub fn push(&mut self, l: f64, r: f64) -> Option<(f64, f64)> {
        let [a, b] = &mut self.lines;
        a.push(l);
        b.push(r);
        let out = (!self.skip).then(|| (a.dot(&self.taps), b.dot(&self.taps)));
        self.skip = !self.skip;
        out
    }
}

/// Mono 1:2 interpolator (zero stuffing, gain 2).
#[derive(Debug, Clone)]
pub struct Interpolator2 {
    taps: Vec<(usize, f64)>,
    line: Line,
}

impl Default for Interpolator2 {
    fn default() -> Self {
        Self::new()
    }
}

impl Interpolator2 {
    pub fn new() -> Self {
        Self {
            taps: nonzero(),
            line: Line::new(),
        }
    }

    fn step(&mut self, x: f64) -> f64 {
        self.line.push(x);
        2.0 * self.line.dot(&self.taps)
    }

    pub fn push(&mut self, x: f64) -> [f64; 2] {
        [self.step(x), self.step(0.0)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::f64::consts::TAU;

    #[test]
    fn half_band_is_symmetric_normalised_and_half_zero() {
        let h = half_band();
        assert_eq!(h[CENTER], 0.5);
        for (n, v) in h.iter().enumerate() {
            assert_eq!(*v, h[TAPS - 1 - n], "symmetric at {n}");
            let off = n.abs_diff(CENTER);
            if off != 0 && off % 2 == 0 {
                assert_eq!(*v, 0.0);
            }
        }
        let odd: f64 = (0..TAPS)
            .filter(|n| n.abs_diff(CENTER) % 2 == 1)
            .map(|n| h[n])
            .sum();
        assert!((odd - 0.5).abs() < 1e-14, "{odd}");
        assert!((h.iter().sum::<f64>() - 1.0).abs() < 1e-14);
        assert_eq!(nonzero().len(), 41);
        // The window tapers to the ends.
        assert!(h[0].abs() < 1e-5 && h[1].abs() < h[CENTER - 1].abs());
        assert!((bessel_i0(0.0) - 1.0).abs() < 1e-15);
        assert!((bessel_i0(1.0) - 1.2660658777520084).abs() < 1e-14);
    }

    fn decimate(hz: f64, n: usize) -> Vec<f64> {
        let mut d = Decimator2::new();
        (0..n)
            .filter_map(|i| {
                let x = (TAU * hz * i as f64 / 96_000.0).sin();
                d.push(x, -x).map(|(l, r)| {
                    assert_eq!(l, -r);
                    l
                })
            })
            .collect()
    }

    #[test]
    fn decimator_passes_1_khz_and_rejects_30_khz() {
        let y = decimate(1000.0, 9600);
        assert_eq!(y.len(), 4800);
        // Linear phase: output m is input 2m delayed by 39 samples.
        let mut worst = 0.0f64;
        for (m, v) in y.iter().enumerate().skip(TAPS) {
            let t = (2 * m) as f64 - CENTER as f64;
            worst = worst.max((v - (TAU * 1000.0 * t / 96_000.0).sin()).abs());
        }
        assert!(worst < 1e-5, "{worst}");
        let alias = decimate(30_000.0, 9600);
        let peak = alias.iter().skip(TAPS).fold(0.0f64, |a, v| a.max(v.abs()));
        assert!(20.0 * peak.log10() < -100.0, "{peak}");
        let dc: Vec<f64> = {
            let mut d = Decimator2::new();
            (0..400)
                .filter_map(|_| d.push(0.25, 0.25))
                .map(|(l, _)| l)
                .collect()
        };
        assert!(dc.iter().skip(TAPS).all(|v| (v - 0.25).abs() < 1e-14));
    }

    #[test]
    fn interpolator_has_unity_gain_and_rejects_images() {
        let mut it = Interpolator2::new();
        let y: Vec<f64> = (0..4800)
            .flat_map(|m| it.push((TAU * 1000.0 * m as f64 / 48_000.0).sin()))
            .collect();
        assert_eq!(y.len(), 9600);
        let mut worst = 0.0f64;
        for (n, v) in y.iter().enumerate().skip(2 * TAPS) {
            let t = n as f64 - CENTER as f64;
            worst = worst.max((v - (TAU * 1000.0 * t / 96_000.0).sin()).abs());
        }
        // Passband error and the 47 kHz image together stay below −100 dB.
        assert!(worst < 1e-5, "{worst}");
        let mut dc = Interpolator2::new();
        let out: Vec<f64> = (0..200).flat_map(|_| dc.push(0.5)).collect();
        assert!(out.iter().skip(2 * TAPS).all(|v| (v - 0.5).abs() < 1e-14));
    }
}
