//! Properties under random input (the S2 fuzz harness, design note §4):
//! block-size invariance (§3.5: 32/64/97/256 with changes at identical sample
//! indices) and a seeded fuzz of parameters, values and block splits. The
//! `fuzz` CI job raises `IEM_FUZZ_ITERS` and seeds `IEM_FUZZ_SEED` per run.

use iem_dsp::eq::{BANDS, Band, BandKind, EqParams, Equalizer, response_db};
use iem_dsp::meter::PeakMeter;
use iem_dsp::pan::{StereoGain, gains, mono_downmix};
use iem_dsp::sanitize::Trips;

struct Rng(u64);

impl Rng {
    fn bits(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn unit(&mut self) -> f64 {
        (self.bits() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, a: f64, b: f64) -> f64 {
        a + (b - a) * self.unit()
    }
    fn below(&mut self, n: usize) -> usize {
        (self.bits() % n as u64) as usize
    }
    fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }
}

fn env(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn rng() -> Rng {
    Rng(env("IEM_FUZZ_SEED", 0x5eed_2026) | 1)
}

fn iterations(default: u64) -> u64 {
    env("IEM_FUZZ_ITERS", default)
}

const KINDS: [BandKind; 4] = [
    BandKind::HighPass,
    BandKind::LowShelf,
    BandKind::Peak,
    BandKind::HighShelf,
];
const WEIRD: [f64; 5] = [f64::NAN, f64::INFINITY, -1.0, 0.0, 1e300];

fn value(rng: &mut Rng, lo: f64, hi: f64) -> f64 {
    if rng.chance(0.03) {
        WEIRD[rng.below(WEIRD.len())]
    } else {
        rng.range(lo, hi)
    }
}

fn random_params(rng: &mut Rng) -> EqParams {
    let mut p = EqParams::standard_flat();
    for b in &mut p.bands {
        *b = Band {
            kind: KINDS[rng.below(4)],
            enabled: rng.chance(0.7),
            freq_hz: 2f64.powf(value(rng, 0.0, 16.0)),
            gain_lin: if rng.chance(0.05) {
                0.0
            } else {
                10f64.powf(value(rng, -2.0, 1.0))
            },
            bw_oct: value(rng, 0.0, 6.0),
        };
    }
    p.global_gain = value(rng, 0.0, 2.0);
    p
}

fn noise(rng: &mut Rng, n: usize) -> Vec<f64> {
    (0..n).map(|_| rng.range(-1.0, 1.0)).collect()
}

/// Runs `input` through an EQ in blocks of `block`, applying each change
/// exactly at its sample index.
fn run(input: &[f64], block: usize, start: &EqParams, changes: &[(usize, EqParams)]) -> Vec<f64> {
    let mut eq = Equalizer::<1>::new(start, 96_000.0);
    let mut out = input.to_vec();
    let mut pos = 0;
    while pos < out.len() {
        for (at, p) in changes {
            if *at == pos {
                eq.set(p);
            }
        }
        let next = changes
            .iter()
            .map(|c| c.0)
            .filter(|a| *a > pos)
            .min()
            .unwrap_or(usize::MAX);
        let end = (pos + block).min(next).min(out.len());
        eq.process([&mut out[pos..end]]);
        pos = end;
    }
    out
}

#[test]
fn eq_output_does_not_depend_on_the_block_size() {
    let mut r = rng();
    let input = noise(&mut r, 12_000);
    let mut a = EqParams::standard_flat();
    a.bands[2] = Band {
        kind: BandKind::Peak,
        enabled: true,
        freq_hz: 1000.0,
        gain_lin: 2.0,
        bw_oct: 1.0,
    };
    let mut b = a;
    b.bands[2].freq_hz = 3000.0;
    b.bands[2].gain_lin = 0.3;
    b.bands[0].enabled = true;
    let mut c = b;
    c.bands[2].enabled = false;
    c.bands[4].enabled = true;
    c.bands[4].gain_lin = 3.0;
    let mut d = c;
    d.global_gain = 0.5;
    d.bands[1] = Band {
        kind: BandKind::LowShelf,
        enabled: true,
        freq_hz: 150.0,
        gain_lin: 0.0,
        bw_oct: 0.0,
    };
    let changes = [(100, b), (1000, c), (1500, a), (5003, d), (5004, b)];
    let whole = run(&input, input.len(), &a, &changes);
    for block in [32, 64, 97, 256, 1] {
        let got = run(&input, block, &a, &changes);
        let err = got
            .iter()
            .zip(&whole)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0, f64::max);
        assert!(err <= 1e-12, "block {block}: {err:e}");
    }
    assert_ne!(whole, input);
}

#[test]
fn fuzz_eq_never_panics_and_valid_input_stays_finite() {
    let mut r = rng();
    let rates = [44_100.0, 48_000.0, 96_000.0];
    for _ in 0..iterations(60) {
        let fs = rates[r.below(3)];
        let p = random_params(&mut r);
        let mut eq = Equalizer::<2>::new(&p, fs);
        let mut trips = Trips::default();
        let mut meter = PeakMeter::<2>::new();
        for _ in 0..8 {
            if r.chance(0.5) {
                let q = random_params(&mut r);
                let valid = q.bands.iter().all(Band::is_valid)
                    && q.global_gain.is_finite()
                    && q.global_gain >= 0.0;
                assert_eq!(eq.set(&q), valid);
                let db = response_db(&eq.params(), fs, r.range(0.0, fs));
                assert!(!db.is_nan() && db >= -300.0);
            }
            let n = 1 + r.below(300);
            let (mut left, mut right) = (noise(&mut r, n), noise(&mut r, n));
            let hostile = r.chance(0.1);
            if hostile {
                let (at, v) = (r.below(n), WEIRD[r.below(WEIRD.len())]);
                left[at] = v;
            }
            eq.process([left.as_mut_slice(), right.as_mut_slice()]);
            meter.observe([left.as_slice(), right.as_slice()]);
            if trips.check([left.as_mut_slice(), right.as_mut_slice()]) {
                eq.reset();
            } else {
                assert!(left.iter().chain(&right).all(|x| x.is_finite()));
            }
            if !hostile {
                assert!(
                    right.iter().all(|x| x.is_finite()),
                    "valid input gave a non-finite sample"
                );
            }
        }
        assert!(meter.take().iter().all(|m| !m.is_nan()));
        assert!(eq.params().bands.iter().all(Band::is_valid));
        assert_eq!(eq.params().bands.len(), BANDS);
    }
}

#[test]
fn fuzz_pan_and_gains_stay_bounded() {
    let mut r = rng();
    for _ in 0..iterations(60) * 20 {
        let p = value(&mut r, -1.5, 1.5);
        let (l, rr) = gains(p);
        assert!(
            (0.0..=1.4143).contains(&l) && (0.0..=1.4143).contains(&rr),
            "{p}"
        );
        assert!(mono_downmix(1.0, 1.0, l, rr).abs() <= 1.5);
        let mut g = StereoGain::new(96_000.0, r.range(0.0, 4.0), r.chance(0.5), p);
        g.set(r.range(0.0, 4.0), r.chance(0.5), value(&mut r, -1.0, 1.0));
        for _ in 0..r.below(2000) {
            let (a, b) = g.tick();
            assert!((0.0..=5.7).contains(&a) && (0.0..=5.7).contains(&b));
        }
    }
}
