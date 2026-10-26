// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! The checkers of what comes back, in songplayer's loop-check vocabulary
//! (its `asio_loop.rs`): a `mismatch`, a `dropout`, a `slip` of k samples
//! (positive: the return comes later, samples were inserted), `no_return`;
//! meters `checked` and `blind` (samples judged, samples with nothing to
//! judge). A run of bad samples of one kind is one event; its samples are
//! counted beside it. Both checkers take samples by position (`check(stamp,
//! samples)`: `samples[i]` is position `stamp + i`), allocate nothing and
//! never panic.
//!
//! [`CounterCheck`] reads [`super::source::Source::Counter`] back: each
//! sample names a position, so its lag (position − named position, mod
//! 65 536) is the path's delay in samples. The first lag that holds for
//! [`CONFIRM`] samples in a row locks; before that every sample is blind.
//! Locked, a sample at the lag is good; zero where the counter is not is a
//! `dropout` (a hole reads 0); a value that is no counter word is a
//! `mismatch`; another lag that holds for [`CONFIRM`] samples is a
//! `slip(k)` and the new lock, one that does not is a `mismatch`. The lock
//! is never dropped: a stream that stops is one long dropout. `no_return`
//! reads true while no lock came within the first `no_return_after`
//! samples. The counter carries 16 bits only in a 24- or 32-bit flow.
//!
//! [`SineCheck`] reads a sine of a known frequency ω (radians a sample).
//! With `c = 2 cos ω`, a sine has `x[n] − c·x[n−1] + x[n−2] = 0` whatever
//! its level and phase; one whose amplitude moves linearly (a fade) leaves
//! a residual that is itself a sine at ω, so the check reads the same
//! filter twice: `x[n] − 2c·x[n−1] + (2 + c²)·x[n−2] − 2c·x[n−3] + x[n−4]`,
//! exactly 0 for a clean sine and for a linear fade of it. A residual over
//! the bound is a `discontinuity`: a repeated, lost or changed sample, a
//! hole. The bound is the noise floor ([`SINE_FLOOR_LSB`] of the flow's
//! encoding: truncation noise reaches at most 16 LSB through the filter) or
//! −60 dB of the sine's recent peak (its largest |x|, decaying with a time
//! constant of [`SINE_HOLD_S`]), whichever is higher, so the kinks where a
//! fade starts or ends (2·Δslope·sin ω) stay under it: the HIL test
//! signal's 500 ms fade-in and 50 ms fade-out are no discontinuity. One bad
//! sample spoils five residuals; a run ends after three good ones. Samples
//! whose window holds nothing above the floor are blind. Known limits: a
//! repeat at the crest of a sine below about 700 Hz at 96 kHz (its error
//! is A·(1 − cos ω)) or of a quiet one (under the floor), a hole at an
//! exact zero crossing, and a glitch within 50 ms after a far louder
//! passage, can hide under the bound.

/// The samples in a row a lag must hold to lock or to slip.
pub const CONFIRM: u32 = 4;
/// A sine's residual over this share of its recent peak is a discontinuity
/// (−60 dB).
pub const SINE_RESIDUAL: f64 = 1e-3;
/// The sine's noise floor, LSB of the flow's encoding.
pub const SINE_FLOOR_LSB: f64 = 32.0;
/// The time constant of the sine's recent peak, s.
pub const SINE_HOLD_S: f64 = 0.05;

/// A counter check's counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CounterStats {
    /// Samples judged against the lock.
    pub checked: u64,
    /// Samples before the lock: nothing to judge.
    pub blind: u64,
    /// Mismatch runs and their samples.
    pub mismatch: u64,
    pub mismatch_frames: u64,
    /// Dropout runs and their samples.
    pub dropout: u64,
    pub dropout_frames: u64,
    /// Slips and the last one's size (positive: the return comes later).
    pub slips: u64,
    pub last_slip: i32,
    /// The lag in use (samples, mod 65 536), once locked.
    pub lag: Option<u16>,
    /// No lock within the check's `no_return_after` samples.
    pub no_return: bool,
}

/// A bad run of the counter: its kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Run {
    Clean,
    Mismatch,
    Dropout,
}

/// The counter's checker.
#[derive(Debug, Clone)]
pub struct CounterCheck {
    no_return_after: u64,
    lock: Option<u16>,
    /// Another lag and its samples in a row, not judged yet.
    pending: Option<(u16, u32)>,
    run: Run,
    stats: CounterStats,
}

impl CounterCheck {
    /// `no_return_after`: blind samples before `no_return` (the caller's
    /// 10 s at its rate, as songplayer's).
    pub fn new(no_return_after: u64) -> Self {
        CounterCheck {
            no_return_after,
            lock: None,
            pending: None,
            run: Run::Clean,
            stats: CounterStats::default(),
        }
    }

    pub fn stats(&self) -> CounterStats {
        CounterStats {
            lag: self.lock,
            no_return: self.lock.is_none() && self.stats.blind >= self.no_return_after,
            ..self.stats
        }
    }

    /// Judges `samples`, `samples[i]` at position `stamp + i`.
    pub fn check(&mut self, stamp: u64, samples: &[i32]) {
        for (i, &v) in samples.iter().enumerate() {
            // The position mod 65 536, and the counter word the sample
            // names (0 included), if it is one.
            let pos = stamp.wrapping_add(i as u64) as u16;
            let named = (v & 0xff == 0 && v >> 24 == 0).then_some((v >> 8) as u16);
            let Some(lock) = self.lock else {
                self.stats.blind += 1;
                self.pending = match named {
                    Some(c) => self.held(pos.wrapping_sub(c)),
                    None => None,
                };
                if let Some((lag, CONFIRM)) = self.pending {
                    self.lock = Some(lag);
                    self.pending = None;
                }
                continue;
            };
            let Some(c) = named else {
                self.bad(Run::Mismatch);
                continue;
            };
            let lag = pos.wrapping_sub(c);
            if lag == lock {
                self.good();
            } else if self.pending.is_some_and(|(l, _)| l == lag) || c != 0 {
                // Another lag (a zero only continues one already seen: else
                // it is a hole).
                self.other_lag(lock, lag);
            } else {
                self.bad(Run::Dropout);
            }
        }
    }

    /// The pending lag after one more sample at `lag`.
    fn held(&self, lag: u16) -> Option<(u16, u32)> {
        match self.pending {
            Some((l, n)) if l == lag => Some((l, n + 1)),
            _ => Some((lag, 1)),
        }
    }

    /// A sample at a lag other than the lock's: it holds for [`CONFIRM`] in
    /// a row and is a slip, or an earlier other lag fails as a mismatch.
    fn other_lag(&mut self, lock: u16, lag: u16) {
        if self.pending.is_some_and(|(l, _)| l != lag) {
            self.flush();
        }
        let held = self.held(lag);
        self.pending = held;
        if let Some((_, CONFIRM)) = held {
            self.pending = None;
            self.lock = Some(lag);
            self.run = Run::Clean;
            self.stats.checked += u64::from(CONFIRM);
            self.stats.slips += 1;
            self.stats.last_slip = i32::from(lag.wrapping_sub(lock) as i16);
        }
    }

    fn good(&mut self) {
        self.flush();
        self.stats.checked += 1;
        self.run = Run::Clean;
    }

    fn bad(&mut self, kind: Run) {
        self.flush();
        self.stats.checked += 1;
        self.count(kind, 1);
    }

    /// The pending other lag failed: its samples are a mismatch.
    fn flush(&mut self) {
        if let Some((_, n)) = self.pending.take() {
            self.stats.checked += u64::from(n);
            self.count(Run::Mismatch, u64::from(n));
        }
    }

    /// `n` bad samples of `kind`: a new event unless its run goes on.
    fn count(&mut self, kind: Run, n: u64) {
        let (events, frames) = match kind {
            Run::Dropout => (&mut self.stats.dropout, &mut self.stats.dropout_frames),
            _ => (&mut self.stats.mismatch, &mut self.stats.mismatch_frames),
        };
        if self.run != kind {
            *events += 1;
        }
        *frames += n;
        self.run = kind;
    }
}

/// A sine check's counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SineStats {
    /// Residuals judged.
    pub checked: u64,
    /// Samples with nothing above the floor around them.
    pub blind: u64,
    /// Runs of residuals over the bound, and their samples.
    pub discontinuities: u64,
    pub discontinuity_frames: u64,
}

/// Good residuals in a row that end a bad run.
const RUN_END: u8 = 3;
/// The samples before the one judged that the residual reads.
const WINDOW: u8 = 4;

/// The sine's checker.
#[derive(Debug, Clone)]
pub struct SineCheck {
    /// The residual's weights: 4 cos ω and 2 + 4 cos² ω.
    a: f64,
    b: f64,
    /// The noise floor, in left-justified units.
    floor: f64,
    /// The recent peak's decay a sample (its time constant [`SINE_HOLD_S`]).
    decay: f64,
    /// The recent peak: the largest |x| so far, decaying.
    peak: f64,
    /// The position the next call continues at.
    next: Option<u64>,
    /// x[n−1] … x[n−4], and how many of them are filled.
    hist: [f64; 4],
    filled: u8,
    /// In a bad run, and the good residuals since its last bad one.
    in_run: bool,
    good: u8,
    stats: SineStats,
}

impl SineCheck {
    /// A sine of `hz` at `rate`, carried in `bits` (16, 24 or 32).
    pub fn new(hz: f64, rate: u32, bits: u8) -> Self {
        let rate = f64::from(rate.max(1));
        let c = 2.0 * (std::f64::consts::TAU * hz / rate).cos();
        SineCheck {
            a: 2.0 * c,
            b: 2.0 + c * c,
            floor: SINE_FLOOR_LSB * 2f64.powi(32 - i32::from(bits.clamp(1, 32))),
            decay: 1.0 - 1.0 / (SINE_HOLD_S * rate),
            peak: 0.0,
            next: None,
            hist: [0.0; 4],
            filled: 0,
            in_run: false,
            good: 0,
            stats: SineStats::default(),
        }
    }

    pub fn stats(&self) -> SineStats {
        self.stats
    }

    /// Judges `samples`, `samples[i]` at position `stamp + i`; a call whose
    /// stamp does not continue the last one starts afresh (four samples to
    /// fill the window).
    pub fn check(&mut self, stamp: u64, samples: &[i32]) {
        if self.next != Some(stamp) {
            self.filled = 0;
            self.in_run = false;
        }
        self.next = Some(stamp.wrapping_add(samples.len() as u64));
        for &v in samples {
            let x = f64::from(v);
            let h = self.hist;
            if self.filled < WINDOW {
                self.filled += 1;
            } else if h.iter().chain([&x]).all(|s| s.abs() <= self.floor) {
                self.stats.blind += 1;
                self.in_run = false;
            } else {
                let residual = x + h[3] - self.a * (h[0] + h[2]) + self.b * h[1];
                self.judge(residual.abs());
            }
            self.hist = [x, h[0], h[1], h[2]];
            // After the judgement: a bad sample never raises its own bound.
            self.peak = x.abs().max(self.peak * self.decay);
        }
    }

    /// One residual: over the bound starts or extends a bad run, and
    /// [`RUN_END`] good ones in a row end it.
    fn judge(&mut self, residual: f64) {
        self.stats.checked += 1;
        if residual > self.floor.max(self.peak * SINE_RESIDUAL) {
            self.stats.discontinuity_frames += 1;
            if !self.in_run {
                self.stats.discontinuities += 1;
            }
            self.in_run = true;
            self.good = 0;
        } else if self.in_run {
            self.good += 1;
            if self.good >= RUN_END {
                self.in_run = false;
            }
        }
    }
}

#[cfg(test)]
mod tests;
