//! Callback telemetry of a real-time audio stream (S1a design note §4). The
//! driver's callback thread is the only writer and records without locks or
//! allocation; any other thread reads snapshots. Portable: the ASIO host
//! (Windows) feeds it, the tests run everywhere.

use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering::Relaxed};

/// Histogram resolution: 1 µs buckets up to 5 ms, then one overflow bucket.
pub const BUCKETS: usize = 5_001;
const BUCKET_NS: u64 = 1_000;
/// The first callbacks of a stream prime the driver's buffers (possibly inside
/// `start()`); they are counted but never judged late, missed or gapped.
pub const WARMUP: u64 = 8;
/// −50 dBFS, the band-activity threshold (program spec §4.2).
pub const ACTIVITY_THRESHOLD: f64 = 0.003_162_277_660_168_379;
const NO_POSITION: i64 = i64::MIN;

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
    reopen: AtomicBool,
    input_peak: AtomicU64,
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
            reopen: AtomicBool::new(false),
            input_peak: AtomicU64::new(0),
        }
    }

    pub fn period_ns(&self) -> u64 {
        self.period_ns
    }

    pub fn callbacks(&self) -> u64 {
        self.callbacks.load(Relaxed)
    }

    /// At the entry of callback: `entry_ns` on the host clock (> 0), the
    /// driver's sample position when it reported one.
    pub fn on_callback(&self, entry_ns: u64, position: Option<i64>) {
        let n = self.callbacks.fetch_add(1, Relaxed);
        let prev = self.last_ns.swap(entry_ns, Relaxed);
        if n == 0 {
            self.first_ns.store(entry_ns, Relaxed);
        } else if n >= WARMUP {
            let dt = entry_ns.saturating_sub(prev);
            self.interval.record(dt);
            match classify(dt, self.period_ns) {
                Gap::Missed => self.missed.fetch_add(1, Relaxed),
                Gap::Late => self.late.fetch_add(1, Relaxed),
                Gap::OnTime => 0,
            };
        }
        if let Some(pos) = position {
            let before = self.last_pos.swap(pos, Relaxed);
            self.last_pos_ns.store(entry_ns, Relaxed);
            if before == NO_POSITION {
                self.first_pos.store(pos, Relaxed);
                self.first_pos_ns.store(entry_ns, Relaxed);
            } else if n >= WARMUP && pos.wrapping_sub(before) != self.frames {
                self.position_gaps.fetch_add(1, Relaxed);
            }
        }
    }

    /// At the exit of a callback: how long it took.
    pub fn on_done(&self, duration_ns: u64) {
        self.duration.record(duration_ns);
        if duration_ns > self.period_ns {
            self.overruns.fetch_add(1, Relaxed);
        }
    }

    /// The largest input |sample| of one callback (linear, 0..=1).
    pub fn on_input_peak(&self, peak: f64) {
        if peak.is_finite() {
            // Non-negative finite f64 bit patterns order like their values.
            self.input_peak.fetch_max(peak.abs().to_bits(), Relaxed);
        }
    }

    /// The largest input peak since the last call.
    pub fn take_input_peak(&self) -> f64 {
        f64::from_bits(self.input_peak.swap(0, Relaxed))
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
        if matches!(sel, selector::RESET_REQUEST | selector::BUFFER_SIZE_CHANGE) {
            self.reopen.store(true, Relaxed);
        }
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
        self.reopen.swap(false, Relaxed)
    }

    pub fn snapshot(&self) -> Snapshot {
        let first_pos = self.first_pos.load(Relaxed);
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
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    const P: u64 = 333_333; // 32 samples at 96 kHz

    #[test]
    fn periods_and_levels() {
        assert_eq!(period_ns(32, 96_000.0), P);
        assert_eq!(period_ns(64, 96_000.0), 666_667);
        assert_eq!(period_ns(48, 96_000.0), 500_000);
        assert_eq!(dbfs(1.0), 0.0);
        assert!((dbfs(ACTIVITY_THRESHOLD) + 50.0).abs() < 1e-9);
        assert_eq!(dbfs(0.0), -150.0);
        assert_eq!(dbfs(-1.0), -150.0);
        assert_eq!(dbfs(1e-12), -150.0);
    }

    #[test]
    fn classify_boundaries() {
        assert_eq!(classify(P, P), Gap::OnTime);
        assert_eq!(classify(300, 200), Gap::OnTime);
        assert_eq!(classify(301, 200), Gap::Late);
        assert_eq!(classify(399, 200), Gap::Late);
        assert_eq!(classify(400, 200), Gap::Missed);
        assert_eq!(classify(0, 200), Gap::OnTime);
        assert_eq!(classify(2 * P - 1, P), Gap::Late);
        assert_eq!(classify(2 * P, P), Gap::Missed);
    }

    #[test]
    fn drift_needs_a_second_and_has_the_card_sign() {
        assert_eq!(drift_ppm((0, 0), (999_999_999, 96_000), 96_000.0), None);
        assert_eq!(drift_ppm((5, 0), (4, 0), 96_000.0), None);
        assert_eq!(
            drift_ppm((0, 0), (1_000_000_000, 96_000), 96_000.0),
            Some(0.0)
        );
        let fast = drift_ppm((0, 0), (10_000_000_000, 960_096), 96_000.0).unwrap();
        assert!((fast - 100.0).abs() < 1e-6, "{fast}");
        let slow = drift_ppm((1_000_000_000, 10), (3_000_000_000, 10 + 191_904), 96_000.0).unwrap();
        assert!((slow + 500.0).abs() < 1e-6, "{slow}");
    }

    #[test]
    fn histogram_quantiles() {
        let h = Histogram::default();
        assert_eq!(h.snapshot().quantile_ns(0.5), 0);
        for _ in 0..98 {
            h.record(333_400);
        }
        h.record(1_200_000);
        h.record(9_000_000);
        let s = h.snapshot();
        assert_eq!(s.total(), 100);
        assert_eq!(s.max_ns, 9_000_000);
        assert_eq!(s.quantile_ns(0.5), 334_000);
        assert_eq!(s.quantile_ns(0.98), 334_000);
        assert_eq!(s.quantile_ns(0.99), 1_201_000);
        assert_eq!(s.quantile_ns(1.0), 9_000_000);
        assert_eq!(s.quantile_ns(0.0), 334_000);
        assert_eq!(s.summary_us(), [334.0, 1201.0, 9000.0, 9000.0]);
        let one = Histogram::default();
        one.record(700);
        assert_eq!(one.snapshot().quantile_ns(0.5), 700);
        assert_eq!(one.snapshot().counts.len(), BUCKETS);
        one.record(u64::MAX);
        assert_eq!(one.snapshot().counts[BUCKETS - 1], 1);
    }

    #[test]
    fn warmup_callbacks_are_counted_but_not_judged() {
        let t = Telemetry::new(32, 96_000.0);
        let mut at = 1_000;
        for i in 0..WARMUP {
            t.on_callback(at, Some(i as i64 * 7));
            at += 10 * P;
        }
        let s = t.snapshot();
        assert_eq!(
            (
                s.callbacks,
                s.late,
                s.missed,
                s.position_gaps,
                s.interval.total()
            ),
            (WARMUP, 0, 0, 0, 0)
        );
        assert_eq!(s.first_callback_ns, 1_000);
    }

    #[test]
    fn late_missed_and_position_gaps_are_counted() {
        let t = Telemetry::new(32, 96_000.0);
        let mut at = 1_000;
        let mut pos = 0;
        for _ in 0..WARMUP {
            t.on_callback(at, Some(pos));
            at += P;
            pos += 32;
        }
        t.on_callback(at, Some(pos)); // on time
        at += P * 3 / 2 + 1;
        pos += 32;
        t.on_callback(at, Some(pos)); // late
        at += 2 * P;
        pos += 64;
        t.on_callback(at, Some(pos)); // missed, one gap
        at += P;
        t.on_callback(at, None); // no position: no gap judged
        let s = t.snapshot();
        assert_eq!(
            (s.callbacks, s.late, s.missed, s.position_gaps),
            (WARMUP + 4, 1, 1, 1)
        );
        assert_eq!(s.interval.total(), 4);
        assert_eq!(t.callbacks(), WARMUP + 4);
    }

    #[test]
    fn overruns_are_longer_than_a_period() {
        let t = Telemetry::new(32, 96_000.0);
        t.on_done(P);
        t.on_done(P + 1);
        t.on_done(10);
        let s = t.snapshot();
        assert_eq!(s.overruns, 1);
        assert_eq!(s.duration.total(), 3);
        assert_eq!(s.period_ns, P);
        assert_eq!(t.period_ns(), P);
    }

    #[test]
    fn drift_is_anchored_after_the_warmup_burst() {
        // 48 samples at 96 kHz: exactly 500 µs. The priming callbacks arrive
        // in a burst inside start(); the card then runs exactly on time.
        const Q: u64 = 500_000;
        let t = Telemetry::new(48, 96_000.0);
        assert_eq!(t.snapshot().drift_ppm, None);
        let mut pos = 0;
        for i in 0..WARMUP {
            t.on_callback(1_000 + i, Some(pos));
            pos += 48;
        }
        assert_eq!(t.snapshot().drift_ppm, None);
        for k in 0..=2_000 {
            t.on_callback(1_007 + (k + 1) * Q, Some(pos));
            pos += 48;
        }
        let s = t.snapshot();
        assert_eq!(
            (s.drift_ppm, s.position_gaps, s.late, s.missed),
            (Some(0.0), 0, 0, 0)
        );
        // Callbacks without a position leave the drift's end where it was.
        t.on_callback(1_007 + 2_002 * Q, None);
        assert_eq!(t.snapshot().drift_ppm, Some(0.0));
    }

    #[test]
    fn a_missing_position_is_not_a_gap_but_a_jump_after_warmup_is() {
        let t = Telemetry::new(32, 96_000.0);
        let mut at = 1_000;
        let mut pos = 0;
        for _ in 0..WARMUP {
            t.on_callback(at, Some(pos));
            at += P;
            pos += 32;
        }
        // The first judged callback jumps by two buffers: one gap.
        t.on_callback(at, Some(pos + 32));
        pos += 64;
        at += P;
        assert_eq!(t.snapshot().position_gaps, 1);
        t.on_callback(at, Some(pos));
        pos += 32;
        at += P;
        // One callback without a position: the next one has nothing to compare with.
        t.on_callback(at, None);
        pos += 32;
        at += P;
        t.on_callback(at, Some(pos));
        pos += 32;
        at += P;
        t.on_callback(at, Some(pos));
        assert_eq!(t.snapshot().position_gaps, 1);
        at += P;
        t.on_callback(at, Some(pos + 96));
        assert_eq!(t.snapshot().position_gaps, 2);
    }

    #[test]
    fn input_peaks_keep_the_maximum_until_taken() {
        let t = Telemetry::new(32, 96_000.0);
        assert_eq!(t.take_input_peak(), 0.0);
        t.on_input_peak(0.25);
        t.on_input_peak(0.5);
        t.on_input_peak(0.125);
        t.on_input_peak(f64::NAN);
        assert_eq!(t.take_input_peak(), 0.5);
        assert_eq!(t.take_input_peak(), 0.0);
        t.on_input_peak(-0.75);
        t.on_input_peak(f64::INFINITY);
        assert_eq!(t.take_input_peak(), 0.75);
    }

    #[test]
    fn driver_messages_are_answered() {
        use selector::*;
        for s in [
            ENGINE_VERSION,
            RESET_REQUEST,
            BUFFER_SIZE_CHANGE,
            RESYNC_REQUEST,
            LATENCIES_CHANGED,
            SUPPORTS_TIME_INFO,
            OVERLOAD,
        ] {
            assert_eq!(reply(SELECTOR_SUPPORTED, s), 1, "{s}");
        }
        for s in [SELECTOR_SUPPORTED, SUPPORTS_TIME_CODE, 9, 0] {
            assert_eq!(reply(SELECTOR_SUPPORTED, s), 0, "{s}");
        }
        let answers: Vec<i32> = [
            ENGINE_VERSION,
            RESET_REQUEST,
            BUFFER_SIZE_CHANGE,
            RESYNC_REQUEST,
            LATENCIES_CHANGED,
            SUPPORTS_TIME_INFO,
            SUPPORTS_TIME_CODE,
            OVERLOAD,
            99,
        ]
        .into_iter()
        .map(|s| reply(s, 0))
        .collect();
        assert_eq!(answers, [2, 1, 0, 1, 1, 1, 0, 0, 0]);
    }

    #[test]
    fn driver_messages_are_counted_and_resets_request_a_reopen() {
        use selector::*;
        let t = Telemetry::new(32, 96_000.0);
        assert_eq!(t.driver_message(ENGINE_VERSION, 0), 2);
        assert_eq!(t.driver_message(SELECTOR_SUPPORTED, OVERLOAD), 1);
        assert!(!t.take_reopen());
        assert_eq!(t.driver_message(RESET_REQUEST, 0), 1);
        assert!(t.take_reopen());
        assert!(!t.take_reopen());
        assert_eq!(t.driver_message(BUFFER_SIZE_CHANGE, 64), 0);
        assert!(t.take_reopen());
        assert_eq!(t.driver_message(RESYNC_REQUEST, 0), 1);
        assert_eq!(t.driver_message(LATENCIES_CHANGED, 0), 1);
        assert!(!t.take_reopen());
        assert_eq!(t.driver_message(OVERLOAD, 0), 0);
        assert_eq!(t.driver_message(OVERLOAD, 0), 0);
        assert_eq!(t.rate_changes(), 0);
        t.on_rate_change();
        assert_eq!(t.rate_changes(), 1);
        let s = t.snapshot();
        assert_eq!(
            (
                s.resets,
                s.buffer_size_changes,
                s.resyncs,
                s.latency_changes,
                s.overloads,
                s.rate_changes
            ),
            (1, 1, 1, 1, 2, 1)
        );
    }

    #[test]
    fn activity_needs_consecutive_seconds_above_minus_50_dbfs() {
        let mut g = ActivityGuard::new(3);
        assert!(!g.observe(0.01));
        assert!(!g.observe(0.01));
        assert!(!g.observe(ACTIVITY_THRESHOLD));
        assert!(!g.observe(0.01));
        assert!(!g.observe(0.01));
        assert!(g.observe(0.01));
        assert!(g.observe(1.0));
        assert!(!g.observe(0.0));
        let mut now = ActivityGuard::new(1);
        assert!(now.observe(ACTIVITY_THRESHOLD * 1.001));
    }
}
