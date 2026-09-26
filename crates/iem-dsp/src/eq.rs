//! ReaEQ-parity EQ (A12; S1b `peak_bw`, `hp_bw`, `shelf_bw`, `hp_gain`,
//! `eq_edge`). Each band is a TPT state-variable filter (Simper, linear
//! trapezoidal) fed the RBJ design's f0, Q and A, so its transfer function is
//! ReaEQ's biquad while parameter changes stay glitch-free (X15, 20 ms).

use core::f64::consts::{FRAC_PI_2, LN_2, PI, TAU};

use crate::ramp::{EQ_MS, Ramp, samples};

/// Bands per EQ (the site's fixed layout).
pub const BANDS: usize = 5;
/// ReaEQ's frequency range.
pub const FREQ_MIN: f64 = 20.0;
pub const FREQ_MAX: f64 = 24_000.0;
/// Measured cap: f0 ≤ 0.49·fs (`eq_edge` top at 44.1 and 48 kHz).
pub const NYQUIST_FRACTION: f64 = 0.49;
/// Measured floor (`eq_edge` bw 0 renders as 0.01) and ReaEQ's top.
pub const BW_MIN: f64 = 0.01;
pub const BW_MAX: f64 = 4.0;
/// ReaEQ's top gain (+12.04 dB).
pub const GAIN_MAX: f64 = 4.0;
/// Shelf gain floor (a shelf at gain 0 was not measured).
pub const SHELF_GAIN_MIN: f64 = 1e-6;
/// RBJ shelf slope cap (`shelf_bw`: S = min(1/bw², 1.2)).
pub const SHELF_SLOPE_MAX: f64 = 1.2;
/// Floor of the gain ramp in dB (gain 0 is −∞ dB).
const RAMP_DB_MIN: f64 = -120.0;
/// Filter states below this are flushed to zero (denormals).
const DENORMAL: f64 = 1e-30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandKind {
    HighPass,
    LowShelf,
    Peak,
    HighShelf,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    pub kind: BandKind,
    pub enabled: bool,
    pub freq_hz: f64,
    /// Linear gain: 0 = −∞ dB (a notch on a peak band), 4 = +12.04 dB.
    pub gain_lin: f64,
    pub bw_oct: f64,
}

impl Band {
    /// A band's values must be finite and not negative (the engine caps fields, §2.3).
    pub fn is_valid(&self) -> bool {
        [self.freq_hz, self.gain_lin, self.bw_oct]
            .iter()
            .all(|x| x.is_finite() && *x >= 0.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EqParams {
    pub bands: [Band; BANDS],
    pub global_gain: f64,
}

impl EqParams {
    /// ReaEQ's standard layout (HP, LS, peak, peak, HS) at its default
    /// frequencies and bandwidths, every band off and flat.
    pub const fn standard_flat() -> Self {
        const fn off(kind: BandKind, freq_hz: f64, bw_oct: f64) -> Band {
            Band {
                kind,
                enabled: false,
                freq_hz,
                gain_lin: 1.0,
                bw_oct,
            }
        }
        Self {
            bands: [
                off(BandKind::HighPass, 80.20834168547682, 2.0),
                off(BandKind::LowShelf, 200.3077623397404, 2.0),
                off(BandKind::Peak, 801.9398157380639, 1.0),
                off(BandKind::Peak, 2996.2342070275295, 1.0),
                off(BandKind::HighShelf, 8016.061124722856, 2.0),
            ],
            global_gain: 1.0,
        }
    }
}

/// TPT SVF coefficients: y = m0·x + m1·band + m2·low.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Coefs {
    g: f64,
    k: f64,
    a1: f64,
    a2: f64,
    a3: f64,
    m0: f64,
    m1: f64,
    m2: f64,
}

fn svf(g: f64, k: f64, m0: f64, m1: f64, m2: f64) -> Coefs {
    let a1 = 1.0 / (1.0 + g * (g + k));
    let a2 = g * a1;
    Coefs {
        g,
        k,
        a1,
        a2,
        a3: g * a2,
        m0,
        m1,
        m2,
    }
}

/// The band's filter at ReaEQ's effective values.
fn design(b: &Band, fs: f64) -> Coefs {
    let f = b
        .freq_hz
        .clamp(FREQ_MIN, FREQ_MAX)
        .min(NYQUIST_FRACTION * fs);
    let bw = b.bw_oct.clamp(BW_MIN, BW_MAX);
    let gain = b.gain_lin.min(GAIN_MAX);
    let g = (PI * f / fs).tan();
    match b.kind {
        BandKind::HighPass | BandKind::Peak => {
            let w0 = TAU * f / fs;
            let warp = if w0 <= FRAC_PI_2 {
                w0 / w0.sin()
            } else {
                FRAC_PI_2
            };
            // 1/Q from α = sin w0 · sinh(ln2/2 · bw · warp) (`alpha_oct`).
            let q_inv = 2.0 * (LN_2 / 2.0 * bw * warp).sinh();
            match b.kind {
                BandKind::HighPass => svf(g, q_inv, 1.0, -q_inv, -1.0),
                _ if gain <= 0.0 => svf(g, q_inv, 1.0, -q_inv, 0.0),
                _ => {
                    let a = gain.sqrt();
                    let k = q_inv / a;
                    svf(g, k, 1.0, k * (a * a - 1.0), 0.0)
                }
            }
        }
        BandKind::LowShelf | BandKind::HighShelf => {
            let a = gain.max(SHELF_GAIN_MIN).sqrt();
            let s = (1.0 / (bw * bw)).min(SHELF_SLOPE_MAX);
            let k = ((a + 1.0 / a) * (1.0 / s - 1.0) + 2.0).max(0.0).sqrt();
            if b.kind == BandKind::LowShelf {
                svf(g / a.sqrt(), k, 1.0, k * (a - 1.0), a * a - 1.0)
            } else {
                svf(g * a.sqrt(), k, a * a, k * (1.0 - a) * a, 1.0 - a * a)
            }
        }
    }
}

impl Coefs {
    /// |H| at `freq` (bilinear: s = j·tan(π f/fs)/g).
    fn magnitude(&self, fs: f64, freq: f64) -> f64 {
        let t = (PI * freq / fs).tan() / self.g;
        let nr = self.m0 + self.m2 - self.m0 * t * t;
        let ni = (self.m0 * self.k + self.m1) * t;
        let dr = 1.0 - t * t;
        let di = self.k * t;
        ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt()
    }

    /// One sample through the SVF with state `s` = [ic1, ic2].
    fn tick(&self, s: &mut [f64; 2], x: f64) -> f64 {
        let [ic1, ic2] = *s;
        let v3 = x - ic2;
        let v1 = self.a1 * ic1 + self.a2 * v3;
        let v2 = ic2 + self.a2 * ic1 + self.a3 * v3;
        *s = [flush(2.0 * v1 - ic1), flush(2.0 * v2 - ic2)];
        self.m0 * x + self.m1 * v1 + self.m2 * v2
    }
}

fn flush(x: f64) -> f64 {
    if x.abs() < DENORMAL { 0.0 } else { x }
}

/// Magnitude response of `params` in dB at `freq_hz` (the UI curve, F11): the
/// engine's exact designs, evaluated analytically. Frequencies are limited to
/// 0.499·fs; −300 dB floor.
pub fn response_db(params: &EqParams, sample_rate: f64, freq_hz: f64) -> f64 {
    let f = freq_hz.min(0.499 * sample_rate).max(0.0);
    let mag = params
        .bands
        .iter()
        .filter(|b| b.enabled)
        .fold(params.global_gain, |m, b| {
            m * design(b, sample_rate).magnitude(sample_rate, f)
        });
    if mag > 1e-15 {
        20.0 * mag.log10()
    } else {
        -300.0
    }
}

/// One band with its smoothed parameters and per-channel state.
#[derive(Debug, Clone, Copy, PartialEq)]
struct BandState<const CH: usize> {
    target: Band,
    freq: Ramp,
    gain: Ramp,
    bw: Ramp,
    wet: Ramp,
    coefs: Coefs,
    state: [[f64; 2]; CH],
    len: u32,
}

fn freq_key(b: &Band) -> f64 {
    b.freq_hz.max(FREQ_MIN).log2()
}

fn gain_key(b: &Band) -> f64 {
    if b.gain_lin > 0.0 {
        (20.0 * b.gain_lin.log10()).max(RAMP_DB_MIN)
    } else {
        RAMP_DB_MIN
    }
}

fn bw_key(b: &Band) -> f64 {
    b.bw_oct.max(BW_MIN).log2()
}

impl<const CH: usize> BandState<CH> {
    fn new(b: Band, fs: f64, len: u32) -> Self {
        Self {
            target: b,
            freq: Ramp::new(freq_key(&b), len),
            gain: Ramp::new(gain_key(&b), len),
            bw: Ramp::new(bw_key(&b), len),
            wet: Ramp::new(if b.enabled { 1.0 } else { 0.0 }, len),
            coefs: design(&b, fs),
            state: [[0.0; 2]; CH],
            len,
        }
    }

    fn bypassed(&self) -> bool {
        self.wet.value() == 0.0 && !self.wet.is_moving()
    }

    fn set(&mut self, b: Band, fs: f64) {
        if b.kind != self.target.kind || self.bypassed() {
            let wet = self.wet;
            *self = Self::new(b, fs, self.len);
            // A bypassed band re-enters from zero state through the crossfade.
            self.wet = wet;
        } else {
            self.target = b;
            self.freq.set(freq_key(&b));
            self.gain.set(gain_key(&b));
            self.bw.set(bw_key(&b));
            if !self.moving() {
                self.coefs = design(&b, fs);
            }
        }
        self.wet.set(if b.enabled { 1.0 } else { 0.0 });
    }

    fn moving(&self) -> bool {
        self.freq.is_moving() || self.gain.is_moving() || self.bw.is_moving()
    }

    /// Advance the ramps one sample; recompute the filter while it moves.
    fn advance(&mut self, fs: f64) {
        self.wet.tick();
        if self.moving() {
            let f = self.freq.tick().exp2();
            let db = self.gain.tick();
            let bw = self.bw.tick().exp2();
            self.coefs = if self.moving() {
                let gain_lin = 10f64.powf(db / 20.0);
                design(
                    &Band {
                        freq_hz: f,
                        gain_lin,
                        bw_oct: bw,
                        ..self.target
                    },
                    fs,
                )
            } else {
                design(&self.target, fs)
            };
        }
        if self.bypassed() {
            self.state = [[0.0; 2]; CH];
        }
    }

    fn tick(&mut self, ch: usize, x: f64) -> f64 {
        let wet = self.wet.value();
        if wet == 0.0 {
            return x;
        }
        let Some(s) = self.state.get_mut(ch) else {
            return x;
        };
        let y = self.coefs.tick(s, x);
        if wet == 1.0 { y } else { x + wet * (y - x) }
    }
}

/// A five-band EQ on `CH` channels (1 for mono inputs, 2 for buses) sharing one
/// set of coefficients.
#[derive(Debug, Clone, PartialEq)]
pub struct Equalizer<const CH: usize> {
    sample_rate: f64,
    bands: [BandState<CH>; BANDS],
    global: Ramp,
}

impl<const CH: usize> Equalizer<CH> {
    /// An EQ resting at `params` (no ramp); invalid bands start flat and off.
    pub fn new(params: &EqParams, sample_rate: f64) -> Self {
        let len = samples(EQ_MS, sample_rate);
        let mut bands = EqParams::standard_flat()
            .bands
            .map(|b| BandState::new(b, sample_rate, len));
        for (state, b) in bands.iter_mut().zip(params.bands) {
            if b.is_valid() {
                *state = BandState::new(b, sample_rate, len);
            }
        }
        let global = if params.global_gain.is_finite() && params.global_gain >= 0.0 {
            params.global_gain
        } else {
            1.0
        };
        Self {
            sample_rate,
            bands,
            global: Ramp::new(global, len),
        }
    }

    /// New targets; they ramp over 20 ms. Invalid bands and a non-finite or
    /// negative global gain are ignored (false).
    pub fn set(&mut self, params: &EqParams) -> bool {
        let mut ok = true;
        for (state, b) in self.bands.iter_mut().zip(params.bands) {
            if b.is_valid() {
                state.set(b, self.sample_rate);
            } else {
                ok = false;
            }
        }
        if params.global_gain.is_finite() && params.global_gain >= 0.0 {
            self.global.set(params.global_gain);
        } else {
            ok = false;
        }
        ok
    }

    /// The current targets.
    pub fn params(&self) -> EqParams {
        EqParams {
            bands: self.bands.map(|b| b.target),
            global_gain: self.global.target(),
        }
    }

    /// Zero every filter state (node reset after a sanitiser trip, X1).
    /// True while the EQ passes its input through unchanged: every band
    /// bypassed and the global gain resting at exactly 1 (the engine then
    /// skips `process`, which would return the input bit for bit).
    pub fn is_identity(&self) -> bool {
        !self.global.is_moving()
            && self.global.value() == 1.0
            && self.bands.iter().all(BandState::bypassed)
    }

    pub fn reset(&mut self) {
        for b in &mut self.bands {
            b.state = [[0.0; 2]; CH];
        }
    }

    /// Processes the block in place (length: the shortest channel).
    pub fn process(&mut self, mut chans: [&mut [f64]; CH]) {
        let n = chans.iter().map(|c| c.len()).min().unwrap_or(0);
        for i in 0..n {
            for b in &mut self.bands {
                b.advance(self.sample_rate);
            }
            let gain = self.global.tick();
            for (c, ch) in chans.iter_mut().enumerate() {
                if let Some(x) = ch.get_mut(i) {
                    let mut v = *x;
                    for b in &mut self.bands {
                        v = b.tick(c, v);
                    }
                    *x = v * gain;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 96_000.0;
    /// 20 ms at 96 kHz. Tests loop over this constant, not over `samples()`, so
    /// a wrong ramp length fails them at once instead of running for minutes.
    const RAMP: usize = 1920;

    fn slot(kind: BandKind) -> usize {
        match kind {
            BandKind::HighPass => 0,
            BandKind::LowShelf => 1,
            BandKind::Peak => 2,
            BandKind::HighShelf => 4,
        }
    }

    fn band(kind: BandKind, freq_hz: f64, gain_lin: f64, bw_oct: f64) -> Band {
        Band {
            kind,
            enabled: true,
            freq_hz,
            gain_lin,
            bw_oct,
        }
    }

    fn single(b: Band) -> EqParams {
        let mut p = EqParams::standard_flat();
        p.bands[slot(b.kind)] = b;
        p
    }

    fn impulse_response(p: &EqParams, fs: f64, n: usize) -> Vec<f64> {
        let mut x = vec![0.0; n];
        x[0] = 1.0;
        Equalizer::<1>::new(p, fs).process([&mut x]);
        x
    }

    /// Independent reference: ReaEQ's RBJ biquad as S1b measured it
    /// (scripts/golden/analyze.py `rbj`, `alpha_oct`, `alpha_shelf`), direct form I.
    fn rbj_ir(b: &Band, fs: f64, n: usize) -> Vec<f64> {
        let w0 = 2.0 * PI * b.freq_hz / fs;
        let c = w0.cos();
        let a = b.gain_lin.sqrt();
        let al = match b.kind {
            BandKind::Peak | BandKind::HighPass => {
                let warp = if w0 <= PI / 2.0 {
                    w0 / w0.sin()
                } else {
                    PI / 2.0
                };
                w0.sin() * (2f64.ln() / 2.0 * b.bw_oct * warp).sinh()
            }
            _ => {
                let s = (1.0 / b.bw_oct.powi(2)).min(1.2);
                w0.sin() / 2.0 * ((a + 1.0 / a) * (1.0 / s - 1.0) + 2.0).sqrt()
            }
        };
        let r = 2.0 * a.sqrt() * al;
        let (bb, aa) = match b.kind {
            BandKind::Peak => (
                [1.0 + al * a, -2.0 * c, 1.0 - al * a],
                [1.0 + al / a, -2.0 * c, 1.0 - al / a],
            ),
            BandKind::HighPass => (
                [(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0],
                [1.0 + al, -2.0 * c, 1.0 - al],
            ),
            BandKind::LowShelf => (
                [
                    a * ((a + 1.0) - (a - 1.0) * c + r),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * c),
                    a * ((a + 1.0) - (a - 1.0) * c - r),
                ],
                [
                    (a + 1.0) + (a - 1.0) * c + r,
                    -2.0 * ((a - 1.0) + (a + 1.0) * c),
                    (a + 1.0) + (a - 1.0) * c - r,
                ],
            ),
            BandKind::HighShelf => (
                [
                    a * ((a + 1.0) + (a - 1.0) * c + r),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
                    a * ((a + 1.0) + (a - 1.0) * c - r),
                ],
                [
                    (a + 1.0) - (a - 1.0) * c + r,
                    2.0 * ((a - 1.0) - (a + 1.0) * c),
                    (a + 1.0) - (a - 1.0) * c - r,
                ],
            ),
        };
        let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
        (0..n)
            .map(|i| {
                let x = if i == 0 { 1.0 } else { 0.0 };
                let y = (bb[0] * x + bb[1] * x1 + bb[2] * x2 - aa[1] * y1 - aa[2] * y2) / aa[0];
                (x2, x1, y2, y1) = (x1, x, y1, y);
                y
            })
            .collect()
    }

    fn max_diff(a: &[f64], b: &[f64]) -> f64 {
        a.iter()
            .zip(b)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0, f64::max)
    }

    fn run_silence(eq: &mut Equalizer<2>, n: usize) {
        let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
        eq.process([l.as_mut_slice(), r.as_mut_slice()]);
    }

    #[test]
    fn identity_only_when_every_band_is_bypassed_at_unity() {
        let flat = EqParams::standard_flat();
        let mut eq = Equalizer::<2>::new(&flat, SR);
        assert!(eq.is_identity());
        let x: Vec<f64> = (0..64)
            .map(|i| ((i * 37) % 11) as f64 / 7.0 - 0.6)
            .collect();
        let (mut l, mut r) = (x.clone(), x.clone());
        eq.process([l.as_mut_slice(), r.as_mut_slice()]);
        assert_eq!((&l, &r), (&x, &x));
        let mut on = flat;
        on.bands[2] = band(BandKind::Peak, 1000.0, 2.0, 1.0);
        eq.set(&on);
        assert!(!eq.is_identity());
        run_silence(&mut eq, RAMP);
        assert!(!eq.is_identity());
        on.bands[2].enabled = false;
        eq.set(&on);
        assert!(!eq.is_identity());
        run_silence(&mut eq, RAMP - 1);
        assert!(!eq.is_identity());
        run_silence(&mut eq, 1);
        assert!(eq.is_identity());
        let mut half = flat;
        half.global_gain = 0.5;
        eq.set(&half);
        assert!(!eq.is_identity());
        run_silence(&mut eq, RAMP);
        assert!(!eq.is_identity());
        eq.set(&flat);
        assert!(!eq.is_identity());
        run_silence(&mut eq, RAMP);
        assert!(eq.is_identity());
    }

    #[test]
    fn every_kind_equals_the_rbj_biquad() {
        for fs in [44_100.0, 96_000.0] {
            for kind in [
                BandKind::HighPass,
                BandKind::LowShelf,
                BandKind::Peak,
                BandKind::HighShelf,
            ] {
                for f in [20.0, 1000.0, 15_000.0] {
                    for db in [-12.0, 3.0, 12.0] {
                        for bw in [0.01, 0.4, 2.0, 4.0] {
                            let gain = if kind == BandKind::HighPass {
                                1.0
                            } else {
                                10f64.powf(db / 20.0)
                            };
                            let b = band(kind, f, gain, bw);
                            let err = max_diff(
                                &impulse_response(&single(b), fs, 256),
                                &rbj_ir(&b, fs, 256),
                            );
                            assert!(
                                err < 1e-10,
                                "{kind:?} {f} Hz {db} dB {bw} oct at {fs}: {err:e}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn reaeq_edges_are_reproduced() {
        let at = |b: Band, fs: f64| impulse_response(&single(b), fs, 64);
        // bw 0 renders as 0.01 oct; the gain of an HPF band is ignored; gain tops out at 4.
        assert_eq!(
            at(band(BandKind::Peak, 1000.0, 2.0, 0.0), SR),
            at(band(BandKind::Peak, 1000.0, 2.0, 0.01), SR)
        );
        assert_eq!(
            at(band(BandKind::HighPass, 300.0, 2.86, 2.0), SR),
            at(band(BandKind::HighPass, 300.0, 1.0, 2.0), SR)
        );
        assert_eq!(
            at(band(BandKind::Peak, 1000.0, 9.0, 1.0), SR),
            at(band(BandKind::Peak, 1000.0, 4.0, 1.0), SR)
        );
        assert_eq!(
            at(band(BandKind::Peak, 5.0, 2.0, 9.0), SR),
            at(band(BandKind::Peak, 20.0, 2.0, 4.0), SR)
        );
        // f0 is capped at 0.49·fs: 24 kHz renders like w0 = 0.98π at 44.1 and 48 kHz
        // (golden eq44-edge-top h[0] = 1.0246656184155964 at +6 dB, 1 oct).
        let six = 10f64.powf(6.0 / 20.0);
        let top44 = at(band(BandKind::Peak, 24_000.0, six, 1.0), 44_100.0);
        assert!(
            max_diff(
                &top44,
                &at(band(BandKind::Peak, 24_000.0, six, 1.0), 48_000.0)
            ) < 1e-12
        );
        assert_eq!(
            top44,
            at(band(BandKind::Peak, 0.49 * 44_100.0, six, 1.0), 44_100.0)
        );
        assert!((top44[0] - 1.0246656184155964).abs() < 1e-12);
        // A shelf at gain 0 uses the floor.
        assert_eq!(
            at(band(BandKind::LowShelf, 200.0, 0.0, 1.0), SR),
            at(band(BandKind::LowShelf, 200.0, SHELF_GAIN_MIN, 1.0), SR)
        );
    }

    #[test]
    fn a_peak_at_gain_zero_is_a_notch() {
        // eq_edge gain0 at 44.1 kHz: b = [1, -2c, 1]/(1+α), a2 = (1-α)/(1+α).
        let h = impulse_response(
            &single(band(BandKind::Peak, 1000.0, 0.0, 1.0)),
            44_100.0,
            64,
        );
        assert!((h[0] - 0.9520367506573633).abs() < 1e-13, "{}", h[0]);
        let p = single(band(BandKind::Peak, 1000.0, 0.0, 1.0));
        assert!(response_db(&p, 44_100.0, 1000.0) < -200.0);
        assert!(response_db(&p, 44_100.0, 20.0).abs() < 0.01);
    }

    #[test]
    fn disabled_bands_and_global_gain() {
        let mut p = single(band(BandKind::Peak, 1000.0, 4.0, 1.0));
        p.bands[2].enabled = false;
        p.global_gain = 0.5;
        let h = impulse_response(&p, SR, 8);
        assert_eq!(h, vec![0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        assert!((response_db(&p, SR, 1000.0) - 20.0 * 0.5f64.log10()).abs() < 1e-12);
        p.global_gain = 0.0;
        assert_eq!(response_db(&p, SR, 1000.0), -300.0);
    }

    /// Direct DTFT of a long impulse response.
    fn dtft_db(h: &[f64], fs: f64, f: f64) -> f64 {
        let w = 2.0 * PI * f / fs;
        let (re, im) = h.iter().enumerate().fold((0.0, 0.0), |(re, im), (n, x)| {
            let ph = w * n as f64;
            (re + x * ph.cos(), im - x * ph.sin())
        });
        10.0 * (re * re + im * im).log10()
    }

    #[test]
    fn response_db_is_the_filter_response() {
        let mut p = EqParams::standard_flat();
        p.bands[0] = band(BandKind::HighPass, 120.0, 1.0, 2.0);
        p.bands[1] = band(BandKind::LowShelf, 300.0, 0.5, 1.5);
        p.bands[2] = band(BandKind::Peak, 2500.0, 2.0, 0.4);
        p.bands[4] = band(BandKind::HighShelf, 9000.0, 1.8, 2.0);
        p.global_gain = 0.9;
        for fs in [48_000.0, SR] {
            let h = impulse_response(&p, fs, 1 << 15);
            for f in [20.0, 90.0, 300.0, 1000.0, 2500.0, 7000.0, 20_000.0] {
                let got = response_db(&p, fs, f);
                let want = dtft_db(&h, fs, f);
                assert!((got - want).abs() < 1e-6, "{f} Hz at {fs}: {got} vs {want}");
            }
        }
        // Frequencies at or past Nyquist are evaluated at 0.499·fs.
        assert_eq!(
            response_db(&p, SR, 60_000.0),
            response_db(&p, SR, 0.499 * SR)
        );
        assert_eq!(response_db(&p, SR, -5.0), response_db(&p, SR, 0.0));
    }

    #[test]
    fn a_change_ramps_over_20_ms_and_lands_on_the_exact_design() {
        let from = single(band(BandKind::Peak, 1000.0, 1.0, 1.0));
        let to = single(band(BandKind::Peak, 4000.0, 4.0, 0.5));
        let mut eq = Equalizer::<1>::new(&from, SR);
        assert!(eq.set(&to));
        assert_eq!(eq.params(), to);
        let n = RAMP;
        assert_eq!(samples(EQ_MS, SR), 1920);
        assert_eq!(eq.bands[2].len, 1920);
        let mut x = vec![0.0; n - 1];
        eq.process([&mut x]);
        let b = &eq.bands[2];
        assert!(b.moving());
        assert_ne!(b.coefs, design(&to.bands[2], SR));
        assert_ne!(b.coefs, design(&from.bands[2], SR));
        let mut one = [0.0];
        eq.process([&mut one]);
        assert!(!eq.bands[2].moving());
        assert_eq!(eq.bands[2].coefs, design(&to.bands[2], SR));
    }

    #[test]
    fn halfway_through_a_ramp_every_parameter_is_at_its_log_midpoint() {
        // log2 f, dB and log2 bw ramp linearly: 1 → 4 kHz, 0 → +12.04 dB,
        // 1 → 0.25 oct is 2 kHz, gain 2 (+6.02 dB), 0.5 oct after 10 ms.
        let from = single(band(BandKind::Peak, 1000.0, 1.0, 1.0));
        let to = single(band(BandKind::Peak, 4000.0, 4.0, 0.25));
        let mut eq = Equalizer::<1>::new(&from, SR);
        eq.set(&to);
        let mut x = vec![0.0; RAMP / 2];
        eq.process([&mut x]);
        let got = eq.bands[2].coefs;
        let want = design(&band(BandKind::Peak, 2000.0, 2.0, 0.5), SR);
        for (g, w) in [
            (got.g, want.g),
            (got.k, want.k),
            (got.m1, want.m1),
            (got.a1, want.a1),
        ] {
            assert!((g - w).abs() <= 1e-9 * w.abs(), "{g} vs {w}");
        }
    }

    #[test]
    fn a_bandwidth_change_alone_ramps_in_log2() {
        // 2 → 0.5 oct: after 10 ms log2 bw is halfway, 1 oct.
        let from = single(band(BandKind::Peak, 1000.0, 2.0, 2.0));
        let to = single(band(BandKind::Peak, 1000.0, 2.0, 0.5));
        let mut eq = Equalizer::<1>::new(&from, SR);
        eq.set(&to);
        assert!(eq.bands[2].moving());
        let mut x = vec![0.0; RAMP / 2];
        eq.process([&mut x]);
        let got = eq.bands[2].coefs;
        let want = design(&band(BandKind::Peak, 1000.0, 2.0, 1.0), SR);
        for (g, w) in [(got.k, want.k), (got.m1, want.m1), (got.a1, want.a1)] {
            assert!((g - w).abs() <= 1e-9 * w.abs(), "{g} vs {w}");
        }
        let mut rest = vec![0.0; RAMP / 2];
        eq.process([&mut rest]);
        assert!(!eq.bands[2].moving());
        assert_eq!(eq.bands[2].coefs, design(&to.bands[2], SR));
    }

    #[test]
    fn a_change_below_the_ramp_floor_applies_at_once() {
        // −140 dB and gain 0 (a notch) share the −120 dB ramp key: nothing
        // ramps, so the new design must be taken immediately.
        let from = single(band(BandKind::Peak, 1000.0, 1e-7, 1.0));
        let to = single(band(BandKind::Peak, 1000.0, 0.0, 1.0));
        let mut eq = Equalizer::<1>::new(&from, SR);
        assert!(eq.set(&to));
        assert!(!eq.bands[2].moving());
        assert_ne!(design(&from.bands[2], SR), design(&to.bands[2], SR));
        assert_eq!(eq.bands[2].coefs, design(&to.bands[2], SR));
        let mut x = vec![0.0; 64];
        x[0] = 1.0;
        eq.process([&mut x]);
        assert_eq!(x, impulse_response(&to, SR, 64));
    }

    #[test]
    fn enabling_crossfades_linearly_from_dry_to_wet() {
        // During the 20 ms fade-in, output = (1 − w)·dry + w·filtered with
        // w = (i + 1)/n at sample i; the filtered signal starts from zero state.
        let off = EqParams::standard_flat();
        let mut on = off;
        on.bands[2] = band(BandKind::Peak, 1000.0, 3.0, 1.0);
        let input: Vec<f64> = (0..RAMP)
            .map(|i| ((i * 7919) % 101) as f64 / 50.0 - 1.0)
            .collect();
        let mut filtered = input.clone();
        Equalizer::<1>::new(&on, SR).process([&mut filtered]);
        let mut eq = Equalizer::<1>::new(&off, SR);
        eq.set(&on);
        let mut y = input.clone();
        eq.process([&mut y]);
        for (i, ((got, x), f)) in y.iter().zip(&input).zip(&filtered).enumerate() {
            let w = (i + 1) as f64 / RAMP as f64;
            let want = (1.0 - w) * x + w * f;
            assert!((got - want).abs() < 1e-9, "sample {i}: {got} vs {want}");
        }
        assert_eq!(y[RAMP - 1], filtered[RAMP - 1]);
    }

    #[test]
    fn during_a_ramp_the_parameters_move_monotonically() {
        let from = single(band(BandKind::Peak, 1000.0, 0.25, 1.0));
        let to = single(band(BandKind::Peak, 1000.0, 4.0, 1.0));
        let mut eq = Equalizer::<1>::new(&from, SR);
        eq.set(&to);
        let mut last = response_db(&from, SR, 1000.0);
        for _ in 0..RAMP / 64 {
            let mut x = [0.0; 64];
            eq.process([&mut x]);
            let c = eq.bands[2].coefs;
            let now = 20.0 * c.magnitude(SR, 1000.0).log10();
            assert!(now > last, "gain must rise every block: {now} after {last}");
            last = now;
        }
        assert!((last - 12.0412).abs() < 1e-3);
    }

    #[test]
    fn enabling_crossfades_from_zero_state_and_disabling_bypasses() {
        let off = EqParams::standard_flat();
        let mut on = off;
        on.bands[2] = band(BandKind::Peak, 1000.0, 3.0, 1.0);
        let n = RAMP;
        let input: Vec<f64> = (0..4 * n)
            .map(|i| ((i * 7919) % 101) as f64 / 50.0 - 1.0)
            .collect();
        let mut eq = Equalizer::<1>::new(&off, SR);
        eq.set(&on);
        let mut y = input.clone();
        eq.process([&mut y]);
        // After the fade the band equals a filter started (from zero) at the switch.
        let mut fresh = input.clone();
        Equalizer::<1>::new(&on, SR).process([&mut fresh]);
        assert_eq!(&y[n..], &fresh[n..]);
        assert_ne!(&y[..n], &fresh[..n]);
        assert_ne!(&y[..n], &input[..n]);
        // Disabling: after 20 ms the output is the input, and the state is zero.
        eq.set(&off);
        let mut z = input.clone();
        eq.process([&mut z]);
        assert_eq!(&z[n..], &input[n..]);
        assert_eq!(eq.bands[2].state, [[0.0; 2]]);
        assert_ne!(&z[..n], &input[..n]);
    }

    #[test]
    fn invalid_values_are_ignored_and_reset_clears_state() {
        let good = single(band(BandKind::Peak, 1000.0, 2.0, 1.0));
        let mut eq = Equalizer::<2>::new(&good, SR);
        let mut bad = good;
        bad.bands[2].freq_hz = f64::NAN;
        bad.global_gain = -1.0;
        assert!(!eq.set(&bad));
        assert_eq!(eq.params(), good);
        let mut bad_band = good;
        bad_band.bands[0].bw_oct = -1.0;
        assert!(!eq.set(&bad_band));
        let mut infinite = good;
        infinite.global_gain = f64::INFINITY;
        assert!(!eq.set(&infinite));
        assert_eq!(eq.params(), good);
        let mut rejected = good;
        rejected.bands[2].gain_lin = f64::INFINITY;
        rejected.global_gain = f64::NAN;
        let fallback = Equalizer::<1>::new(&rejected, SR).params();
        assert_eq!(fallback.bands[2], EqParams::standard_flat().bands[2]);
        assert_eq!(fallback.global_gain, 1.0);
        // Reset: afterwards the EQ behaves like a new one.
        let (mut l, mut r) = ([0.3; 50], [-0.2; 50]);
        eq.process([&mut l, &mut r]);
        eq.reset();
        let (mut l2, mut r2) = ([0.1; 20], [0.4; 20]);
        eq.process([&mut l2, &mut r2]);
        let mut fresh = Equalizer::<2>::new(&good, SR);
        let (mut l3, mut r3) = ([0.1; 20], [0.4; 20]);
        fresh.process([&mut l3, &mut r3]);
        assert_eq!((l2, r2), (l3, r3));
    }

    #[test]
    fn stereo_channels_match_mono_and_the_shortest_channel_sets_the_length() {
        let p = single(band(BandKind::LowShelf, 250.0, 2.5, 1.0));
        let src: Vec<f64> = (0..300).map(|i| (i as f64 * 0.37).sin()).collect();
        let mut mono = src.clone();
        Equalizer::<1>::new(&p, SR).process([&mut mono]);
        let (mut l, mut r) = (src.clone(), src[..200].to_vec());
        Equalizer::<2>::new(&p, SR).process([&mut l, &mut r]);
        assert_eq!(&l[..200], &mono[..200]);
        assert_eq!(&r[..], &mono[..200]);
        assert_eq!(&l[200..], &src[200..]);
    }

    #[test]
    fn a_kind_change_restarts_the_band() {
        let mut eq = Equalizer::<1>::new(&single(band(BandKind::Peak, 1000.0, 2.0, 1.0)), SR);
        let mut x = [0.5; 32];
        eq.process([&mut x]);
        let mut p = eq.params();
        p.bands[2] = band(BandKind::HighShelf, 5000.0, 2.0, 1.0);
        eq.set(&p);
        assert_eq!(eq.bands[2].state, [[0.0; 2]]);
        assert_eq!(eq.bands[2].coefs, design(&p.bands[2], SR));
        assert!(!eq.bands[2].moving());
    }

    #[test]
    fn denormal_states_flush_to_zero() {
        assert_eq!(flush(1e-31), 0.0);
        assert_eq!(flush(-1e-31), 0.0);
        assert_eq!(flush(1e-29), 1e-29);
        // Only magnitudes strictly below the threshold are flushed.
        assert_eq!(flush(DENORMAL), DENORMAL);
        assert_eq!(flush(-DENORMAL), -DENORMAL);
        let mut eq = Equalizer::<1>::new(&single(band(BandKind::Peak, 1000.0, 2.0, 1.0)), SR);
        let mut x = vec![0.0; 200_000];
        x[0] = 1.0;
        eq.process([&mut x]);
        assert_eq!(eq.bands[2].state, [[0.0; 2]]);
        assert!(x[1] != 0.0);
    }

    #[test]
    fn validity_and_the_standard_layout() {
        assert!(band(BandKind::Peak, 1000.0, 0.0, 0.0).is_valid());
        assert!(!band(BandKind::Peak, -1.0, 1.0, 1.0).is_valid());
        assert!(!band(BandKind::Peak, 1000.0, f64::NAN, 1.0).is_valid());
        assert!(!band(BandKind::Peak, 1000.0, 1.0, f64::INFINITY).is_valid());
        let s = EqParams::standard_flat();
        assert_eq!(
            s.bands.map(|b| b.kind),
            [
                BandKind::HighPass,
                BandKind::LowShelf,
                BandKind::Peak,
                BandKind::Peak,
                BandKind::HighShelf
            ]
        );
        assert!(s.bands.iter().all(|b| !b.enabled && b.gain_lin == 1.0));
        assert_eq!(s.bands.map(|b| b.bw_oct), [2.0, 2.0, 1.0, 1.0, 2.0]);
        assert_eq!(s.global_gain, 1.0);
    }
}
