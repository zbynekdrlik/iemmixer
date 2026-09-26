// Part of iem-limiter-mga (GPL-3.0-or-later); see ../src/lib.rs.

//! Fuzz harness (S2 design note §4): random material, limits, toggles and block
//! splits, with hostile values. Properties: never a panic; the limiter never
//! amplifies; while enabled the output stays under the limit; after a
//! non-finite input a node reset restores bounded output. The `fuzz` CI job
//! raises `IEM_FUZZ_ITERS` and seeds `IEM_FUZZ_SEED` per run.

use iem_limiter_mga::Limiter;

struct Rng(u64);

impl Rng {
    fn unit(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, a: f64, b: f64) -> f64 {
        a + (b - a) * self.unit()
    }
    fn below(&mut self, n: usize) -> usize {
        (self.unit() * n as f64) as usize % n
    }
}

fn env(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

const HOSTILE: [f64; 4] = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e300];

#[test]
fn fuzz_the_limiter_never_amplifies_and_holds_its_limit() {
    let mut rng = Rng(env("IEM_FUZZ_SEED", 0x11_2026) | 1);
    for _ in 0..env("IEM_FUZZ_ITERS", 40) {
        let sr = [44_100.0, 48_000.0, 96_000.0][rng.below(3)];
        let mut lim = Limiter::new(sr, rng.range(-8.0, 2.0));
        let mut last_active = 0;
        for _ in 0..12 {
            match rng.below(4) {
                0 => lim.set_limit_db(rng.range(-8.0, 2.0)),
                1 => lim.set_enabled(rng.unit() < 0.5),
                _ => {}
            }
            let n = 1 + rng.below(700);
            let level = rng.range(0.0, 10.0);
            let (mut l, mut r): (Vec<f64>, Vec<f64>) = (0..n)
                .map(|_| (level * rng.range(-1.0, 1.0), level * rng.range(-1.0, 1.0)))
                .unzip();
            let hostile = rng.unit() < 0.05;
            if hostile {
                let (at, v) = (rng.below(n), HOSTILE[rng.below(HOSTILE.len())]);
                l[at] = v;
            }
            let (xl, xr) = (l.clone(), r.clone());
            let enabled_throughout = lim.enabled();
            lim.process(&mut l, &mut r);
            let ceiling = 10f64.powf(lim.limit_db() / 20.0);
            let bad = l.iter().chain(&r).any(|y| !y.is_finite() || y.abs() > 1e6);
            if bad {
                assert!(hostile, "finite input gave a non-finite output");
                lim.reset();
                continue;
            }
            for (y, x) in l.iter().zip(&xl).chain(r.iter().zip(&xr)) {
                assert!(
                    y.abs() <= x.abs() * (1.0 + 1e-12) + 1e-300,
                    "amplified {x} to {y}"
                );
                if enabled_throughout && !hostile {
                    assert!(
                        y.abs() <= ceiling * (1.0 + 1e-12),
                        "{y} over the limit {ceiling}"
                    );
                }
            }
            assert!(lim.active_samples() >= last_active);
            last_active = lim.active_samples();
            assert!(lim.gr_db() <= 0.0 && lim.gr_db() >= -150.0);
        }
    }
}
