// Part of iem-limiter-mga (GPL-3.0-or-later); see ../src/lib.rs.

//! A13 parity: a line-by-line translation of the JSFX (checked against the
//! fixture it translates) equals the port, and the port equals REAPER's renders
//! (S1b `lim-*`: residual < −100 dBFS, spec ≤ 1e-12), in any block size.

#![allow(
    non_snake_case,
    clippy::identity_op,
    clippy::field_reassign_with_default
)]

use iem_limiter_mga::{LINK_PCT, Limiter, Mga, RELEASE_MS, Sliders};
use iem_rpp::golden::{Goldens, s1b_dir};
use iem_rpp::stimulus::hot_material;

const FIXTURE: &str = include_str!("../fixtures/MGA_JSLimiterST");

/// The JSFX lines `Literal` translates, in order (`@init` to the end of `@sample`).
const TRANSLATED: [&str; 40] = [
    "@init",
    "ext_tail_size=-1;",
    "ext_gr_meter = 0;",
    "HOLDTIME = srate/128;",
    "r1Timer = 0;",
    "r2Timer = HOLDTIME/2;",
    "r1TimerO = 0;",
    "r2TimerO = HOLDTIME/2;",
    "gr_meter=1;",
    "gr_meter_decay = exp(1/(1*srate));",
    "@slider",
    "thresh = 10^(slider1/20);",
    "ceiling = 10^(slider4/20);",
    "volume = ceiling/thresh;",
    "release = slider2/1000;",
    "r = exp(-3/(srate*max(release,0.05)));",
    "link = sqrt(slider3*0.01);",
    "@sample",
    "maxSpls=abs(spl0);",
    "(r1Timer+=1) > HOLDTIME ? (r1Timer = 0; max1Block = 0; );",
    "max1Block = max(max1Block,maxSpls);",
    "(r2Timer+=1) > HOLDTIME ? (r2Timer = 0; max2Block = 0; );",
    "max2Block = max(max2Block,maxSpls);",
    "envT = max(max1Block,max2Block);",
    "maxSplsO=abs(spl1);",
    "(r1TimerO+=1) > HOLDTIME ? (r1TimerO = 0; max1BlockO = 0; );",
    "max1BlockO = max(max1BlockO,maxSplsO);",
    "(r2TimerO+=1) > HOLDTIME ? (r2TimerO = 0; max2BlockO = 0; );",
    "max2BlockO = max(max2BlockO,maxSplsO);",
    "envTO = max(max1BlockO,max2BlockO);",
    "env = max(env,envO*link);",
    "envO = max(env*link,envO);",
    "env = env < envT ? envT : envT + r*(env-envT);",
    "(env > thresh) ? gain = (g_meter=(thresh / env))*volume : (g_meter=1; gain=volume;);",
    "envO = envO < envTO ? envTO : envTO + r*(envO-envTO);",
    "(envO > thresh) ? gainO = (g_meterO=(thresh / envO))*volume : (g_meterO=1; gainO=volume;);",
    "spl0*=gain;",
    "spl1*=gainO;",
    "g_meter = min(g_meter,g_meterO);",
    "g_meter < gr_meter ? gr_meter=g_meter : ( gr_meter*=gr_meter_decay; gr_meter>1?gr_meter=1; );",
];

fn max(a: f64, b: f64) -> f64 {
    if a > b { a } else { b }
}

fn min(a: f64, b: f64) -> f64 {
    if a < b { a } else { b }
}

/// The JSFX, one Rust statement per line of `TRANSLATED` (EEL2: every
/// variable is an f64 that starts at 0).
#[derive(Default)]
struct Literal {
    srate: f64,
    HOLDTIME: f64,
    r1Timer: f64,
    r2Timer: f64,
    r1TimerO: f64,
    r2TimerO: f64,
    gr_meter: f64,
    gr_meter_decay: f64,
    thresh: f64,
    ceiling: f64,
    volume: f64,
    release: f64,
    r: f64,
    link: f64,
    maxSpls: f64,
    max1Block: f64,
    max2Block: f64,
    envT: f64,
    maxSplsO: f64,
    max1BlockO: f64,
    max2BlockO: f64,
    envTO: f64,
    env: f64,
    envO: f64,
    gain: f64,
    g_meter: f64,
    gainO: f64,
    g_meterO: f64,
}

impl Literal {
    fn new(srate: f64, slider1: f64, slider2: f64, slider3: f64, slider4: f64) -> Self {
        let mut s = Self {
            srate,
            ..Self::default()
        };
        // @init (ext_tail_size and ext_gr_meter only talk to REAPER)
        s.HOLDTIME = s.srate / 128.0;
        s.r1Timer = 0.0;
        s.r2Timer = s.HOLDTIME / 2.0;
        s.r1TimerO = 0.0;
        s.r2TimerO = s.HOLDTIME / 2.0;
        s.gr_meter = 1.0;
        s.gr_meter_decay = (1.0 / (1.0 * s.srate)).exp();
        // @slider
        s.thresh = 10f64.powf(slider1 / 20.0);
        s.ceiling = 10f64.powf(slider4 / 20.0);
        s.volume = s.ceiling / s.thresh;
        s.release = slider2 / 1000.0;
        s.r = (-3.0 / (s.srate * max(s.release, 0.05))).exp();
        s.link = (slider3 * 0.01).sqrt();
        s
    }

    fn sample(&mut self, mut spl0: f64, mut spl1: f64) -> (f64, f64) {
        self.maxSpls = spl0.abs();
        self.r1Timer += 1.0;
        if self.r1Timer > self.HOLDTIME {
            self.r1Timer = 0.0;
            self.max1Block = 0.0;
        }
        self.max1Block = max(self.max1Block, self.maxSpls);
        self.r2Timer += 1.0;
        if self.r2Timer > self.HOLDTIME {
            self.r2Timer = 0.0;
            self.max2Block = 0.0;
        }
        self.max2Block = max(self.max2Block, self.maxSpls);
        self.envT = max(self.max1Block, self.max2Block);
        self.maxSplsO = spl1.abs();
        self.r1TimerO += 1.0;
        if self.r1TimerO > self.HOLDTIME {
            self.r1TimerO = 0.0;
            self.max1BlockO = 0.0;
        }
        self.max1BlockO = max(self.max1BlockO, self.maxSplsO);
        self.r2TimerO += 1.0;
        if self.r2TimerO > self.HOLDTIME {
            self.r2TimerO = 0.0;
            self.max2BlockO = 0.0;
        }
        self.max2BlockO = max(self.max2BlockO, self.maxSplsO);
        self.envTO = max(self.max1BlockO, self.max2BlockO);
        self.env = max(self.env, self.envO * self.link);
        self.envO = max(self.env * self.link, self.envO);
        self.env = if self.env < self.envT {
            self.envT
        } else {
            self.envT + self.r * (self.env - self.envT)
        };
        if self.env > self.thresh {
            self.g_meter = self.thresh / self.env;
            self.gain = self.g_meter * self.volume;
        } else {
            self.g_meter = 1.0;
            self.gain = self.volume;
        }
        self.envO = if self.envO < self.envTO {
            self.envTO
        } else {
            self.envTO + self.r * (self.envO - self.envTO)
        };
        if self.envO > self.thresh {
            self.g_meterO = self.thresh / self.envO;
            self.gainO = self.g_meterO * self.volume;
        } else {
            self.g_meterO = 1.0;
            self.gainO = self.volume;
        }
        spl0 *= self.gain;
        spl1 *= self.gainO;
        self.g_meter = min(self.g_meter, self.g_meterO);
        if self.g_meter < self.gr_meter {
            self.gr_meter = self.g_meter;
        } else {
            self.gr_meter *= self.gr_meter_decay;
            if self.gr_meter > 1.0 {
                self.gr_meter = 1.0;
            }
        }
        (spl0, spl1)
    }
}

#[test]
fn the_fixture_is_the_jsfx_the_literal_translation_follows() {
    let lines: Vec<&str> = FIXTURE.lines().map(str::trim).collect();
    let start = lines.iter().position(|l| *l == "@init").unwrap();
    let end = lines.iter().position(|l| *l == "@block").unwrap();
    let code: Vec<&str> = lines[start..end]
        .iter()
        .copied()
        .filter(|l| !l.is_empty() && !l.starts_with("//"))
        .collect();
    assert_eq!(code, TRANSLATED);
    assert!(FIXTURE.contains("Copyright (C) 2008  Michael Gruhn"));
    assert!(FIXTURE.contains("either version 3 of the License, or"));
    assert!(FIXTURE.contains("desc:MGA JS Limiter"));
}

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
}

/// Bursts of noise at random levels (up to +18 dBFS), silences and steady tones.
fn material(rng: &mut Rng, n: usize) -> (Vec<f64>, Vec<f64>) {
    let (mut l, mut r) = (Vec::with_capacity(n), Vec::with_capacity(n));
    while l.len() < n {
        let len = 1 + (rng.unit() * 3000.0) as usize;
        let (gl, gr) = if rng.unit() < 0.2 {
            (0.0, 0.0)
        } else {
            (rng.range(0.0, 8.0), rng.range(0.0, 8.0))
        };
        for i in 0..len {
            let tone = (i as f64 * 0.03).sin();
            l.push(gl * rng.range(-1.0, 1.0));
            r.push(gr * tone);
        }
    }
    l.truncate(n);
    r.truncate(n);
    (l, r)
}

#[test]
fn the_port_equals_the_literal_translation() {
    let mut rng = Rng(0x1ea5_2026);
    let mut worst = 0.0f64;
    for case in 0..24 {
        let srate = [44_100.0, 48_000.0, 96_000.0][case % 3];
        let s = Sliders {
            threshold_db: rng.range(-30.0, 0.0),
            release_ms: rng.range(0.0, 500.0),
            link_pct: rng.range(0.0, 100.0),
            ceiling_db: rng.range(-6.0, 0.0),
        };
        let mut lit = Literal::new(
            srate,
            s.threshold_db,
            s.release_ms,
            s.link_pct,
            s.ceiling_db,
        );
        let mut port = Mga::new(srate, s);
        let (l, r) = material(&mut rng, 20_000);
        for (x0, x1) in l.iter().zip(&r) {
            let (a0, a1) = lit.sample(*x0, *x1);
            let (b0, b1) = port.tick(*x0, *x1);
            worst = worst
                .max((a0 - b0).abs())
                .max((a1 - b1).abs())
                .max((lit.gr_meter - port.gr_meter()).abs());
        }
    }
    assert!(worst <= 1e-12, "{worst:e}");
    println!("golden limiter literal translation vs port: max {worst:.3e} (≤ 1e-12)");
}

fn interleave(l: &[f64], r: &[f64]) -> Vec<f64> {
    l.iter().zip(r).flat_map(|(a, b)| [*a, *b]).collect()
}

#[test]
fn the_port_matches_reapers_renders_in_any_block_size() {
    let g = Goldens::open(&s1b_dir()).unwrap();
    let mut n = 0;
    for file in ["lim-44100", "lim-48000", "lim-96000"] {
        let rate: u32 = file[4..].parse().unwrap();
        let sr = f64::from(rate);
        for c in g.cases(file).unwrap() {
            let p = &c.meta["params"];
            assert_eq!(p["release_ms"].as_f64(), Some(RELEASE_MS));
            assert_eq!(p["link_pct"].as_f64(), Some(LINK_PCT));
            let limit = p["limit_db"].as_f64().unwrap();
            let [l, r] = hot_material(rate, p["seed"].as_u64().unwrap());
            let mut outputs = Vec::new();
            for block in [usize::MAX, 32, 64, 97, 256] {
                let mut lim = Limiter::new(sr, limit);
                let (mut yl, mut yr) = (l.clone(), r.clone());
                for (bl, br) in yl
                    .chunks_mut(block.min(l.len()))
                    .zip(yr.chunks_mut(block.min(l.len())))
                {
                    lim.process(bl, br);
                }
                outputs.push((interleave(&yl, &yr), lim.active_samples()));
            }
            let (y, active) = &outputs[0];
            assert_eq!(y.len(), c.data.len());
            let err = y
                .iter()
                .zip(&c.data)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0, f64::max);
            assert!(err <= 1e-12, "{}: {err:e}", c.name);
            let ceiling = 10f64.powf(limit / 20.0);
            assert!(y.iter().all(|x| x.abs() <= ceiling * (1.0 + 1e-12)));
            for (other, a) in &outputs[1..] {
                assert_eq!(other, y, "{}: block size changed the output", c.name);
                assert_eq!(a, active);
            }
            let want_active = match c.name.as_str() {
                "lim-44100-m6" => 39_690,
                "lim-48000-m6" => 43_199,
                "lim-96000-0" => 86_396,
                _ => 86_400,
            };
            assert_eq!(*active, want_active, "{}", c.name);
            let dbfs = 20.0 * err.max(1e-300).log10();
            println!(
                "golden limiter {}: max {err:.3e} ({dbfs:.0} dBFS), active {active} samples",
                c.name
            );
            n += 1;
        }
    }
    assert_eq!(n, 5);
}
