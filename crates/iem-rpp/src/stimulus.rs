//! Deterministic stimuli. Everything except the log sweep uses IEEE basic
//! operations only (no libm), so S2 regenerates it bit-identically.

use std::f64::consts::PI;

pub fn impulse(len: usize, at: usize, amp: f64) -> Vec<f64> {
    let mut v = vec![0.0; len];
    if let Some(x) = v.get_mut(at) {
        *x = amp;
    }
    v
}

/// Exponential sweep f1 → f2 over `secs` (stored in the bundle; not regenerated).
pub fn log_sweep(rate: u32, f1: f64, f2: f64, secs: f64, amp: f64) -> Vec<f64> {
    let fs = f64::from(rate);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = (fs * secs).round() as usize;
    let k = (f2 / f1).ln();
    (0..n)
        .map(|i| {
            #[allow(clippy::cast_precision_loss)]
            let t = i as f64 / fs;
            amp * (2.0 * PI * f1 * secs / k * ((t / secs * k).exp() - 1.0)).sin()
        })
        .collect()
}

struct XorShift(u64);

impl XorShift {
    /// Uniform in [-1, 1): xorshift64* with the top 53 bits.
    #[allow(clippy::cast_precision_loss)]
    fn next_unit(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        let r = x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11;
        r as f64 / (1u64 << 52) as f64 - 1.0
    }
}

#[allow(clippy::cast_precision_loss)]
fn triangle(i: usize, period: usize) -> f64 {
    let phase = (i % period) as f64 / period as f64;
    4.0 * (phase - 0.5).abs() - 1.0
}

/// One second of limiter material in ten 100 ms segments, peaks up to +12 dBFS:
/// silence; L-only noise +6 dB (link); both +9.5 dB triangle (2 segments);
/// silence (release, 2 segments); a 0.3/0.6/1.2/2.4 staircase (2 segments);
/// R noise +12 dB with L at -12 dB.
pub fn hot_material(rate: u32, seed: u64) -> [Vec<f64>; 2] {
    let n = rate as usize;
    let seg = n / 10;
    let mut rng = XorShift(seed | 1);
    let mut left = vec![0.0; n];
    let mut right = vec![0.0; n];
    for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
        let noise = rng.next_unit();
        let (a, b) = match i / seg {
            0 | 4 | 5 => (0.0, 0.0),
            1 => (2.0 * noise, 0.0),
            2 | 3 => (3.0 * triangle(i, 48), 3.0 * triangle(i, 48)),
            6 | 7 => {
                let step = [0.3, 0.6, 1.2, 2.4][((i - 6 * seg) * 4 / (2 * seg)).min(3)];
                (step * triangle(i, 96), step * triangle(i, 96))
            }
            _ => (0.25 * noise, 4.0 * noise),
        };
        *l = a;
        *r = b;
    }
    [left, right]
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn digest(ch: &[Vec<f64>; 2]) -> String {
        let mut h = Sha256::new();
        for c in ch {
            for x in c {
                h.update(x.to_le_bytes());
            }
        }
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn impulse_places_one_sample() {
        assert_eq!(impulse(4, 2, 0.5), vec![0.0, 0.0, 0.5, 0.0]);
        assert_eq!(impulse(2, 5, 0.5), vec![0.0, 0.0]);
    }

    #[test]
    fn hot_material_matches_the_reference_checksum() {
        // Independent Python implementation, 2026-09-26 (seed 27).
        assert_eq!(
            digest(&hot_material(96_000, 27)),
            "c920f0bd71bbd2ec121fc287d4fd5be137b1868eabe3f5eb52719a0b1928770e"
        );
        assert_eq!(
            digest(&hot_material(48_000, 27)),
            "2bdb3ee5df9d1fc5429ba8e4ee8d82a62d79d0a7a9227b975f9e308b35d12d67"
        );
        assert_eq!(
            digest(&hot_material(44_100, 27)),
            "3bcdb799908cf53fbb1fb1b2521481f0feb31ce159e7791a521d3444dc0916f0"
        );
    }

    #[test]
    fn hot_material_segments_have_their_shape() {
        let [l, r] = hot_material(96_000, 27);
        assert!(l[..9_600].iter().chain(&r[..9_600]).all(|x| *x == 0.0));
        assert!(r[9_600..19_200].iter().all(|x| *x == 0.0));
        assert!(l[9_600..19_200].iter().any(|x| x.abs() > 1.9));
        assert!(r[86_400..].iter().any(|x| x.abs() > 3.9));
        assert!(l.iter().chain(&r).all(|x| x.abs() <= 4.0));
    }

    #[test]
    fn log_sweep_has_the_requested_length_and_bound() {
        let s = log_sweep(96_000, 20.0, 20_000.0, 2.0, 0.25);
        assert_eq!(s.len(), 192_000);
        assert!(s.iter().all(|x| x.abs() <= 0.25));
        assert!(s.iter().any(|x| x.abs() > 0.249));
    }
}
