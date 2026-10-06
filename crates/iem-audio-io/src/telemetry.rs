//! Callback telemetry of a real-time audio stream (S1a design note §4). The
//! driver's callback thread is the only writer and records without locks or
//! allocation; any other thread reads snapshots. Portable: the ASIO host
//! (Windows) feeds it, the tests run everywhere.

use core::cmp::Reverse;
use core::sync::atomic::{
    AtomicI64, AtomicU8, AtomicU64,
    Ordering::{Acquire, Relaxed, Release},
};

/// Histogram resolution: 1 µs buckets up to 5 ms, then one overflow bucket.
pub const BUCKETS: usize = 5_001;
const BUCKET_NS: u64 = 1_000;
/// The first callbacks of a stream prime the driver's buffers (possibly inside
/// `start()`); they are counted but never judged late, missed or gapped.
pub const WARMUP: u64 = 8;
/// −50 dBFS, the band-activity threshold (program spec §4.2).
pub const ACTIVITY_THRESHOLD: f64 = 0.003_162_277_660_168_379;
const NO_POSITION: i64 = i64::MIN;
/// [`Telemetry::take_requests`]: a reset request and a buffer size change.
const RESET_BIT: u8 = 1;
const SIZE_BIT: u8 = 2;

/// ASIO driver-to-host message selectors (asio.h `kAsio…`).
pub mod selector {
    pub const SELECTOR_SUPPORTED: i32 = 1;
    pub const ENGINE_VERSION: i32 = 2;
    pub const RESET_REQUEST: i32 = 3;
    pub const BUFFER_SIZE_CHANGE: i32 = 4;
    pub const RESYNC_REQUEST: i32 = 5;
    pub const LATENCIES_CHANGED: i32 = 6;
    pub const SUPPORTS_TIME_INFO: i32 = 7;
    pub const SUPPORTS_TIME_CODE: i32 = 8;
    pub const OVERLOAD: i32 = 15;
}

/// How one callback interval compares with the period.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gap {
    OnTime,
    /// More than 1.5 periods.
    Late,
    /// At least 2 periods: a whole period passed without a callback.
    Missed,
}

pub fn classify(interval_ns: u64, period_ns: u64) -> Gap {
    if interval_ns >= period_ns.saturating_mul(2) {
        Gap::Missed
    } else if interval_ns.saturating_mul(2) > period_ns.saturating_mul(3) {
        Gap::Late
    } else {
        Gap::OnTime
    }
}

/// One buffer period in nanoseconds.
pub fn period_ns(frames: u32, rate: f64) -> u64 {
    (f64::from(frames) * 1e9 / rate).round() as u64
}

/// The card clock against the host clock over (host ns, sample position)
/// pairs, in ppm: positive when the card runs fast. `None` below 1 s of data.
/// The host time is the callback's entry, not the driver's
/// `TimeInfo.system_time`: a few ppm of entry jitter over a 10 min run.
pub fn drift_ppm(first: (u64, i64), last: (u64, i64), rate: f64) -> Option<f64> {
    let elapsed = last.0.checked_sub(first.0)? as f64 / 1e9;
    if elapsed < 1.0 {
        return None;
    }
    let card = last.1.wrapping_sub(first.1) as f64 / rate;
    Some((card - elapsed) / elapsed * 1e6)
}

/// Level of a linear peak in dBFS (−150 for silence).
pub fn dbfs(peak: f64) -> f64 {
    (20.0 * peak.max(1e-300).log10()).max(-150.0)
}

/// The host's answer to an `asioMessage`. It supports resets (by reopening
/// the driver), resyncs and time info; it never resizes buffers live, so a
/// size change is answered 0 (the driver then asks for a reset).
pub fn reply(sel: i32, value: i32) -> i32 {
    use selector::*;
    match sel {
        SELECTOR_SUPPORTED => i32::from(matches!(
            value,
            ENGINE_VERSION
                | RESET_REQUEST
                | BUFFER_SIZE_CHANGE
                | RESYNC_REQUEST
                | LATENCIES_CHANGED
                | SUPPORTS_TIME_INFO
                | OVERLOAD
        )),
        ENGINE_VERSION => 2,
        RESET_REQUEST | RESYNC_REQUEST | LATENCIES_CHANGED | SUPPORTS_TIME_INFO => 1,
        _ => 0,
    }
}

/// What the driver asked a reopen for since the last take
/// ([`Telemetry::take_requests`]); the owner thread logs it (#9 2026-09-28).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Requested {
    /// `kAsioResetRequest`.
    pub reset: bool,
    /// `kAsioBufferSizeChange` (answered 0: the driver then asks for a reset).
    pub buffer_size: bool,
}

impl Requested {
    /// Either asks for a reopen.
    pub fn any(self) -> bool {
        self.reset || self.buffer_size
    }
}

/// What went wrong at one callback (S1c design note §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlitchKind {
    /// Interval above 1.5 periods.
    Late,
    /// Interval of at least 2 periods.
    Missed,
    /// Callback longer than one period.
    Overrun,
    /// The driver's sample position advanced by other than one buffer, or
    /// stood still.
    PositionGap,
    /// The driver's sample position stepped back.
    PositionBack,
}

impl GlitchKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Late => "late",
            Self::Missed => "missed",
            Self::Overrun => "overrun",
            Self::PositionGap => "position-gap",
            Self::PositionBack => "position-back",
        }
    }

    fn code(self) -> u64 {
        match self {
            Self::Late => 0,
            Self::Missed => 1,
            Self::Overrun => 2,
            Self::PositionGap => 3,
            Self::PositionBack => 4,
        }
    }

    fn from_code(code: u64) -> Self {
        match code {
            1 => Self::Missed,
            2 => Self::Overrun,
            3 => Self::PositionGap,
            4 => Self::PositionBack,
            _ => Self::Late,
        }
    }
}

/// One glitch: the callback's entry on the stream clock (ns) and the interval
/// (late, missed), the callback's duration (overrun), the position's step
/// forward in frames (position gap; 0 when it stood still) or how far it
/// stepped back (position back).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glitch {
    pub kind: GlitchKind,
    pub at_ns: u64,
    pub value: u64,
}

/// Glitches kept between two drains; more are counted as dropped.
pub const GLITCH_CAPACITY: usize = 4_096;
/// The packed slot: the kind's code in the top three bits (five kinds), the
/// value below them.
const VALUE_BITS: u32 = 61;
const VALUE_MASK: u64 = (1 << VALUE_BITS) - 1;

/// Single-producer (the callback) single-consumer (the owner thread) ring of
/// glitches: preallocated, lock-free, never blocking the producer.
pub struct GlitchLog {
    at: Box<[AtomicU64]>,
    packed: Box<[AtomicU64]>,
    head: AtomicU64,
    tail: AtomicU64,
    dropped: AtomicU64,
}

impl GlitchLog {
    pub fn new(capacity: usize) -> Self {
        let slots = || (0..capacity.max(1)).map(|_| AtomicU64::new(0)).collect();
        Self {
            at: slots(),
            packed: slots(),
            head: AtomicU64::new(0),
            tail: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        }
    }

    fn slot(&self, n: u64) -> usize {
        usize::try_from(n % self.at.len() as u64).unwrap_or(0)
    }

    /// Callback thread only.
    pub fn push(&self, g: Glitch) {
        let head = self.head.load(Relaxed);
        if head.wrapping_sub(self.tail.load(Acquire)) >= self.at.len() as u64 {
            self.dropped.fetch_add(1, Relaxed);
            return;
        }
        let i = self.slot(head);
        if let (Some(at), Some(p)) = (self.at.get(i), self.packed.get(i)) {
            at.store(g.at_ns, Relaxed);
            // `+`, not `|`: the kind occupies bits at/above VALUE_BITS and the
            // value is clamped below 2^VALUE_BITS, so they never share a bit.
            // `+` reads back identically and, unlike `|`, has no
            // equivalent-mutant twin.
            p.store(
                (g.kind.code() << VALUE_BITS) + g.value.min(VALUE_MASK),
                Relaxed,
            );
        }
        self.head.store(head.wrapping_add(1), Release);
    }

    /// Owner thread only: moves every glitch since the last drain into `out`,
    /// oldest first.
    pub fn drain(&self, out: &mut Vec<Glitch>) {
        let tail = self.tail.load(Relaxed);
        let head = self.head.load(Acquire);
        let mut n = tail;
        while n != head {
            let i = self.slot(n);
            if let (Some(at), Some(p)) = (self.at.get(i), self.packed.get(i)) {
                let packed = p.load(Relaxed);
                out.push(Glitch {
                    kind: GlitchKind::from_code(packed >> VALUE_BITS),
                    at_ns: at.load(Relaxed),
                    value: packed & VALUE_MASK,
                });
            }
            n = n.wrapping_add(1);
        }
        self.tail.store(head, Release);
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Relaxed)
    }
}

/// Logical processors counted per callback; a higher index goes to `cpu_other`.
pub const CPU_SLOTS: usize = 64;

pub struct Histogram {
    counts: Box<[AtomicU64]>,
    max_ns: AtomicU64,
}

impl Default for Histogram {
    fn default() -> Self {
        Self {
            counts: (0..BUCKETS).map(|_| AtomicU64::new(0)).collect(),
            max_ns: AtomicU64::new(0),
        }
    }
}

impl Histogram {
    pub fn record(&self, ns: u64) {
        let i = usize::try_from(ns / BUCKET_NS)
            .unwrap_or(usize::MAX)
            .min(BUCKETS - 1);
        if let Some(c) = self.counts.get(i) {
            c.fetch_add(1, Relaxed);
        }
        self.max_ns.fetch_max(ns, Relaxed);
    }

    pub fn snapshot(&self) -> HistogramSnapshot {
        HistogramSnapshot {
            counts: self.counts.iter().map(|c| c.load(Relaxed)).collect(),
            max_ns: self.max_ns.load(Relaxed),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistogramSnapshot {
    pub counts: Vec<u64>,
    pub max_ns: u64,
}

impl HistogramSnapshot {
    pub fn total(&self) -> u64 {
        self.counts.iter().sum()
    }

    /// The upper edge of the bucket holding quantile `q` (0 < q ≤ 1), never
    /// above the maximum (the overflow bucket reports the maximum); 0 when
    /// empty.
    pub fn quantile_ns(&self, q: f64) -> u64 {
        let total = self.total();
        if total == 0 {
            return 0;
        }
        let rank = ((q * total as f64).ceil() as u64).clamp(1, total);
        let mut seen = 0;
        for (i, &c) in self.counts.iter().enumerate() {
            seen += c;
            if seen >= rank {
                if i + 1 >= BUCKETS {
                    return self.max_ns;
                }
                return (i as u64 + 1).saturating_mul(BUCKET_NS).min(self.max_ns);
            }
        }
        self.max_ns
    }

    /// p50, p99, p99.9 and the maximum, in µs.
    pub fn summary_us(&self) -> [f64; 4] {
        let us = |ns: u64| ns as f64 / 1e3;
        [
            us(self.quantile_ns(0.5)),
            us(self.quantile_ns(0.99)),
            us(self.quantile_ns(0.999)),
            us(self.max_ns),
        ]
    }
}

/// The counters of a [`Snapshot`] that a control loop polls, without the
/// histograms: [`Telemetry::counters`] reads them without allocating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counters {
    pub callbacks: u64,
    pub late: u64,
    pub missed: u64,
    pub overruns: u64,
    /// The longest callback so far (ns).
    pub max_ns: u64,
}

impl Counters {
    /// `self` followed by `later` (the stream after a reopen): the counts
    /// add up, the longest callback is the longer of the two.
    pub fn plus(self, later: Counters) -> Counters {
        Counters {
            callbacks: self.callbacks.saturating_add(later.callbacks),
            late: self.late.saturating_add(later.late),
            missed: self.missed.saturating_add(later.missed),
            overruns: self.overruns.saturating_add(later.overruns),
            max_ns: self.max_ns.max(later.max_ns),
        }
    }
}

/// Everything one stream recorded, read at one moment.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub period_ns: u64,
    pub callbacks: u64,
    pub late: u64,
    pub missed: u64,
    /// Callbacks that took longer than one period.
    pub overruns: u64,
    /// Sample positions that did not advance by exactly one buffer.
    pub position_gaps: u64,
    /// Host ns from the stream's base to its first callback (0 = none yet).
    pub first_callback_ns: u64,
    pub resets: u64,
    pub resyncs: u64,
    pub latency_changes: u64,
    pub buffer_size_changes: u64,
    pub overloads: u64,
    pub rate_changes: u64,
    pub interval: HistogramSnapshot,
    pub duration: HistogramSnapshot,
    pub drift_ppm: Option<f64>,
    /// (logical processor, callbacks it ran), in processor order.
    pub callback_cpus: Vec<(u32, u64)>,
    pub cpu_other: u64,
    /// The thread id of the first callback after the warm-up (0 = none yet).
    pub callback_thread: u32,
    /// Callbacks after the warm-up on a thread other than `callback_thread`.
    pub thread_switches: u64,
    pub glitches_dropped: u64,
}

pub struct Telemetry {
    period_ns: u64,
    frames: i64,
    rate: f64,
    interval: Histogram,
    duration: Histogram,
    callbacks: AtomicU64,
    late: AtomicU64,
    missed: AtomicU64,
    overruns: AtomicU64,
    position_gaps: AtomicU64,
    first_ns: AtomicU64,
    last_ns: AtomicU64,
    /// The previous callback's position, for the gap check (none after a
    /// callback without one).
    prev_pos: AtomicI64,
    /// The drift's anchor and end: positions after the warm-up only.
    first_pos: AtomicI64,
    first_pos_ns: AtomicU64,
    last_pos: AtomicI64,
    last_pos_ns: AtomicU64,
    resets: AtomicU64,
    resyncs: AtomicU64,
    latency_changes: AtomicU64,
    buffer_size_changes: AtomicU64,
    overloads: AtomicU64,
    rate_changes: AtomicU64,
    /// The reopen requests since the last take: `RESET_BIT`, `SIZE_BIT`.
    reopen: AtomicU8,
    /// Judged glitches for the trace markers (S1c): the callback pushes, the
    /// owner thread drains.
    glitches: GlitchLog,
    /// Callbacks per logical processor (index = processor); a higher index
    /// goes to `cpu_other`.
    cpus: Box<[AtomicU64]>,
    cpu_other: AtomicU64,
    /// The thread id of the first callback after the warm-up (0 = none yet).
    thread: AtomicU64,
    thread_switches: AtomicU64,
}

impl Telemetry {
    pub fn new(frames: u32, rate: f64) -> Self {
        Self {
            period_ns: period_ns(frames, rate),
            frames: i64::from(frames),
            rate,
            interval: Histogram::default(),
            duration: Histogram::default(),
            callbacks: AtomicU64::new(0),
            late: AtomicU64::new(0),
            missed: AtomicU64::new(0),
            overruns: AtomicU64::new(0),
            position_gaps: AtomicU64::new(0),
            first_ns: AtomicU64::new(0),
            last_ns: AtomicU64::new(0),
            prev_pos: AtomicI64::new(NO_POSITION),
            first_pos: AtomicI64::new(NO_POSITION),
            first_pos_ns: AtomicU64::new(0),
            last_pos: AtomicI64::new(NO_POSITION),
            last_pos_ns: AtomicU64::new(0),
            resets: AtomicU64::new(0),
            resyncs: AtomicU64::new(0),
            latency_changes: AtomicU64::new(0),
            buffer_size_changes: AtomicU64::new(0),
            overloads: AtomicU64::new(0),
            rate_changes: AtomicU64::new(0),
            reopen: AtomicU8::new(0),
            glitches: GlitchLog::new(GLITCH_CAPACITY),
            cpus: (0..CPU_SLOTS).map(|_| AtomicU64::new(0)).collect(),
            cpu_other: AtomicU64::new(0),
            thread: AtomicU64::new(0),
            thread_switches: AtomicU64::new(0),
        }
    }

    pub fn period_ns(&self) -> u64 {
        self.period_ns
    }

    pub fn callbacks(&self) -> u64 {
        self.callbacks.load(Relaxed)
    }

    /// At the entry of callback: `entry_ns` on the host clock (> 0), the
    /// driver's sample position when it reported one. Positions count only
    /// after the warm-up (the drift is anchored there, not on the priming
    /// burst); a callback without one leaves nothing to compare the next
    /// position with, so no gap is judged across it. Every judged glitch also
    /// enters the glitch log.
    pub fn on_callback(&self, entry_ns: u64, position: Option<i64>) {
        let n = self.callbacks.fetch_add(1, Relaxed);
        let prev = self.last_ns.swap(entry_ns, Relaxed);
        if n == 0 {
            self.first_ns.store(entry_ns, Relaxed);
        } else if n >= WARMUP {
            let dt = entry_ns.saturating_sub(prev);
            self.interval.record(dt);
            let kind = match classify(dt, self.period_ns) {
                Gap::Missed => {
                    self.missed.fetch_add(1, Relaxed);
                    Some(GlitchKind::Missed)
                }
                Gap::Late => {
                    self.late.fetch_add(1, Relaxed);
                    Some(GlitchKind::Late)
                }
                Gap::OnTime => None,
            };
            if let Some(kind) = kind {
                self.glitches.push(Glitch {
                    kind,
                    at_ns: entry_ns,
                    value: dt,
                });
            }
        }
        let before = self.prev_pos.swap(position.unwrap_or(NO_POSITION), Relaxed);
        if n >= WARMUP
            && let Some(pos) = position
        {
            let step = pos.wrapping_sub(before);
            if before != NO_POSITION && step != self.frames {
                self.position_gaps.fetch_add(1, Relaxed);
                // The value is a magnitude: the kind keeps the direction.
                let kind = if step < 0 {
                    GlitchKind::PositionBack
                } else {
                    GlitchKind::PositionGap
                };
                self.glitches.push(Glitch {
                    kind,
                    at_ns: entry_ns,
                    value: step.unsigned_abs(),
                });
            }
            // The end first: a reader that sees the anchor also sees an end.
            self.last_pos_ns.store(entry_ns, Relaxed);
            self.last_pos.store(pos, Relaxed);
            if self.first_pos.load(Relaxed) == NO_POSITION {
                self.first_pos_ns.store(entry_ns, Relaxed);
                self.first_pos.store(pos, Release);
            }
        }
    }

    /// At the exit of a callback: how long it took.
    pub fn on_done(&self, duration_ns: u64) {
        self.duration.record(duration_ns);
        if duration_ns > self.period_ns {
            self.overruns.fetch_add(1, Relaxed);
            self.glitches.push(Glitch {
                kind: GlitchKind::Overrun,
                at_ns: self.last_ns.load(Relaxed),
                value: duration_ns,
            });
        }
    }

    /// At the entry of a callback, from the host, before
    /// [`Telemetry::on_callback`] of the same callback: the logical processor
    /// it runs on and its thread id (both read without a system call). Every
    /// callback's processor is counted. The priming callbacks may run on the
    /// thread that called `start()`, so the first callback after the warm-up
    /// names the callback thread and only later ones count as switches.
    pub fn on_thread(&self, cpu: u32, thread_id: u32) {
        match usize::try_from(cpu).ok().and_then(|i| self.cpus.get(i)) {
            Some(c) => c.fetch_add(1, Relaxed),
            None => self.cpu_other.fetch_add(1, Relaxed),
        };
        // The callbacks recorded before this one (`on_callback` comes next).
        if self.callbacks.load(Relaxed) < WARMUP {
            return;
        }
        let id = u64::from(thread_id);
        if let Err(first) = self.thread.compare_exchange(0, id, Relaxed, Relaxed)
            && first != id
        {
            self.thread_switches.fetch_add(1, Relaxed);
        }
    }

    /// Owner thread only: the glitches since the last call, oldest first.
    pub fn drain_glitches(&self, out: &mut Vec<Glitch>) {
        self.glitches.drain(out);
    }

    /// Counts one `asioMessage` and answers it ([`reply`]).
    pub fn driver_message(&self, sel: i32, value: i32) -> i32 {
        let counter = match sel {
            selector::RESET_REQUEST => Some(&self.resets),
            selector::BUFFER_SIZE_CHANGE => Some(&self.buffer_size_changes),
            selector::RESYNC_REQUEST => Some(&self.resyncs),
            selector::LATENCIES_CHANGED => Some(&self.latency_changes),
            selector::OVERLOAD => Some(&self.overloads),
            _ => None,
        };
        if let Some(c) = counter {
            c.fetch_add(1, Relaxed);
        }
        let request = match sel {
            selector::RESET_REQUEST => RESET_BIT,
            selector::BUFFER_SIZE_CHANGE => SIZE_BIT,
            _ => 0,
        };
        self.reopen.fetch_or(request, Relaxed);
        reply(sel, value)
    }

    /// The driver reported a sample-rate change (the host never sets one).
    pub fn on_rate_change(&self) {
        self.rate_changes.fetch_add(1, Relaxed);
    }

    pub fn rate_changes(&self) -> u64 {
        self.rate_changes.load(Relaxed)
    }

    /// True once after the driver asked for a reset (or a buffer resize).
    pub fn take_reopen(&self) -> bool {
        self.take_requests().any()
    }

    /// Which reopen requests came since the last take (once each).
    pub fn take_requests(&self) -> Requested {
        let bits = self.reopen.swap(0, Relaxed);
        Requested {
            reset: (bits & RESET_BIT) != 0,
            buffer_size: (bits & SIZE_BIT) != 0,
        }
    }

    /// The polled counters (no allocation, unlike [`Telemetry::snapshot`]).
    pub fn counters(&self) -> Counters {
        Counters {
            callbacks: self.callbacks.load(Relaxed),
            late: self.late.load(Relaxed),
            missed: self.missed.load(Relaxed),
            overruns: self.overruns.load(Relaxed),
            max_ns: self.duration.max_ns.load(Relaxed),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let first_pos = self.first_pos.load(Acquire);
        let drift = if first_pos == NO_POSITION {
            None
        } else {
            drift_ppm(
                (self.first_pos_ns.load(Relaxed), first_pos),
                (self.last_pos_ns.load(Relaxed), self.last_pos.load(Relaxed)),
                self.rate,
            )
        };
        Snapshot {
            period_ns: self.period_ns,
            callbacks: self.callbacks.load(Relaxed),
            late: self.late.load(Relaxed),
            missed: self.missed.load(Relaxed),
            overruns: self.overruns.load(Relaxed),
            position_gaps: self.position_gaps.load(Relaxed),
            first_callback_ns: self.first_ns.load(Relaxed),
            resets: self.resets.load(Relaxed),
            resyncs: self.resyncs.load(Relaxed),
            latency_changes: self.latency_changes.load(Relaxed),
            buffer_size_changes: self.buffer_size_changes.load(Relaxed),
            overloads: self.overloads.load(Relaxed),
            rate_changes: self.rate_changes.load(Relaxed),
            interval: self.interval.snapshot(),
            duration: self.duration.snapshot(),
            drift_ppm: drift,
            callback_cpus: self
                .cpus
                .iter()
                .enumerate()
                .filter_map(|(i, c)| {
                    let n = c.load(Relaxed);
                    (n > 0).then(|| (u32::try_from(i).unwrap_or(u32::MAX), n))
                })
                .collect(),
            cpu_other: self.cpu_other.load(Relaxed),
            callback_thread: u32::try_from(self.thread.load(Relaxed)).unwrap_or(u32::MAX),
            thread_switches: self.thread_switches.load(Relaxed),
            glitches_dropped: self.glitches.dropped(),
        }
    }
}

/// The largest |sample| of every card input since the last take (linear,
/// 0..=1). The callback thread records each input's peak without locks or
/// allocation; the owner thread takes all of them once a second.
pub struct InputPeaks {
    peaks: Box<[AtomicU64]>,
}

impl InputPeaks {
    pub fn new(inputs: usize) -> Self {
        Self {
            peaks: (0..inputs).map(|_| AtomicU64::new(0)).collect(),
        }
    }

    /// One callback's peak of input `index` (from 0). Non-finite peaks and
    /// inputs the card does not have are ignored.
    pub fn record(&self, index: usize, peak: f64) {
        if let Some(slot) = self.peaks.get(index)
            && peak.is_finite()
        {
            // Non-negative finite f64 bit patterns order like their values.
            slot.fetch_max(peak.abs().to_bits(), Relaxed);
        }
    }

    /// Every input's peak since the last call; each starts again from 0.
    pub fn take(&self) -> Vec<f64> {
        self.peaks
            .iter()
            .map(|p| f64::from_bits(p.swap(0, Relaxed)))
            .collect()
    }
}

/// The loudest one-second peak of each card input over a run (report).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Loudest {
    peaks: Vec<f64>,
}

impl Loudest {
    pub fn observe(&mut self, peaks: &[f64]) {
        self.peaks.resize(self.peaks.len().max(peaks.len()), 0.0);
        for (m, &p) in self.peaks.iter_mut().zip(peaks) {
            *m = m.max(p);
        }
    }

    /// The loudest input of all (linear).
    pub fn max(&self) -> f64 {
        self.peaks.iter().copied().fold(0.0, f64::max)
    }

    /// The `n` loudest inputs that carried any signal as (index from 0,
    /// linear peak): loudest first, equal peaks by index.
    pub fn top(&self, n: usize) -> Vec<(usize, f64)> {
        let mut hot: Vec<(usize, f64)> = self
            .peaks
            .iter()
            .copied()
            .enumerate()
            .filter(|&(_, p)| p > 0.0)
            .collect();
        hot.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        hot.truncate(n);
        hot
    }
}

/// Band activity on the inputs: `needed` consecutive one-second peaks above
/// −50 dBFS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityGuard {
    run: u32,
    needed: u32,
}

impl ActivityGuard {
    pub fn new(needed: u32) -> Self {
        Self { run: 0, needed }
    }

    /// Feeds one second's peak; true while the band is playing.
    pub fn observe(&mut self, peak: f64) -> bool {
        self.run = if peak > ACTIVITY_THRESHOLD {
            self.run.saturating_add(1)
        } else {
            0
        };
        self.run >= self.needed
    }
}

/// The largest gaps a scan keeps with their times.
pub const LARGEST: usize = 32;

/// The hwlat scan (S1c design note §4.1): a thread that reads the clock in a
/// tight loop sees every stall of its CPU (interrupt, DPC, a higher-priority
/// thread, firmware) as a gap between two reads. Keeps a histogram of the
/// gaps at or above the threshold and the largest ones with their times; no
/// allocation after `new`.
pub struct GapScan {
    threshold_ns: u64,
    gaps: Histogram,
    reads: u64,
    over: u64,
    largest: Vec<(u64, u64)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GapSummary {
    pub reads: u64,
    pub over: u64,
    pub gaps: HistogramSnapshot,
    /// (time of the read before the gap, gap), largest first.
    pub largest: Vec<(u64, u64)>,
}

impl GapScan {
    pub fn new(threshold_ns: u64) -> Self {
        Self {
            threshold_ns,
            gaps: Histogram::default(),
            reads: 0,
            over: 0,
            largest: Vec::with_capacity(LARGEST),
        }
    }

    /// Two consecutive clock reads, in ns since the scan's start.
    pub fn observe(&mut self, prev_ns: u64, now_ns: u64) {
        self.reads += 1;
        let gap = now_ns.saturating_sub(prev_ns);
        if gap < self.threshold_ns {
            return;
        }
        self.over += 1;
        self.gaps.record(gap);
        if self.largest.len() < LARGEST {
            self.largest.push((prev_ns, gap));
        } else if let Some(last) = self.largest.last_mut()
            && gap > last.1
        {
            *last = (prev_ns, gap);
        } else {
            return;
        }
        self.largest.sort_unstable_by_key(|&(_, gap)| Reverse(gap));
    }

    pub fn summary(&self) -> GapSummary {
        GapSummary {
            reads: self.reads,
            over: self.over,
            gaps: self.gaps.snapshot(),
            largest: self.largest.clone(),
        }
    }
}

#[cfg(test)]
#[path = "telemetry_tests.rs"]
mod tests;
