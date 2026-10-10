//! The engine's meter frames and its once-a-second `Status`.

use serde::{Deserialize, Serialize};

/// Peaks since the previous frame (linear): inputs (post-mute), mixes (post
/// volume and mute) and group strips (mix-major: mix `m`, group `g` at
/// `m · groups + g`); limiter GR (dB, 0 while disabled) and X14 active
/// seconds per mix; all in topology order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Meters {
    pub seq: u64,
    pub inputs: Vec<[f32; 2]>,
    pub mixes: Vec<[f32; 2]>,
    pub groups: Vec<[f32; 2]>,
    pub gr_db: Vec<f32>,
    pub limiter_active_s: Vec<f64>,
    pub trips: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Status {
    pub callbacks: u64,
    pub late: u64,
    pub faulted: bool,
    pub process_max_us: f64,
    pub trips: u64,
    pub tap_overruns: u64,
    pub talkback_dropped: u64,
    pub cmd_backlog: u64,
    // S6, additive: the stream's own figures (the ASIO backend; NullRt
    // reports its block and zeros).
    /// The period the driver delivers, measured from its sample positions.
    pub frames: u32,
    /// Callback intervals of two periods or more.
    pub missed: u64,
    /// Callbacks longer than one period.
    pub overruns: u64,
    /// Driver reopens after a reset request or a stall.
    pub resets: u64,
    /// A callback outlived the stop wait: the stream stays allocated (the
    /// guard alarms).
    pub parked: bool,
    /// Started with `--hold` and not armed yet: every output is silent.
    pub held: bool,
    /// Locking the real-time memory failed (logged, never fatal).
    pub lock_failed: bool,
    /// S6, additive: HIL's spare card outputs (the engine opens the site's
    /// `[guard] hil_tx` under the test-signal flag), in that order; empty
    /// otherwise. HIL v1 reads them to prove its test signal reached them,
    /// at its level, and left them.
    pub hil: Vec<HilOut>,
    /// S6 test 5, additive: the D5(b) loopback round-trip, in samples, once
    /// measured (the HIL signal on a spare output, looped back to the matching
    /// spare input in Dante); 0 while none. `loopback_ms` derives the time.
    pub loopback_samples: u64,
    /// S7, additive (design note §3): the callback interval since the stream
    /// opened, 1 µs buckets `[b, b + 1)` below two periods and the overflow
    /// bucket `hist_top_us` (two periods or more: the card's `missed`), as
    /// `[[bucket, count], …]`, non-empty buckets ascending. Absent without a
    /// stream and from an older engine.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub interval_hist: Vec<(u32, u64)>,
    /// S7, additive: the callback's own time, the span `process_max_us`
    /// measures (decode, `process()` and encode on the card; `process()` on
    /// NullRt), in the buckets of `interval_hist`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub process_hist: Vec<(u32, u64)>,
    /// S7, additive: the overflow bucket's index, two periods in µs rounded
    /// up (667 at 32 samples, 96 kHz; at most 1000); 0 without histograms.
    pub hist_top_us: u32,
    /// S7 HIL v2, additive: the last driver reopen, from the old stream's
    /// stop to the new one's measured period, µs; 0 before any (NullRt: 0).
    pub last_reopen_us: u64,
    /// S7 HIL v2, additive: the faulting callback's own time, µs (its entry
    /// to its return, the caught panic included); 0 while not faulted. A
    /// fault sends one last `Status` carrying it before its alarm and
    /// `DriverReleased`.
    pub fault_callback_us: f64,
}

/// One of HIL's spare card outputs in a [`Status`] (S6): its card channel
/// and its peak since the previous `Status` (linear).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HilOut {
    pub tx: u16,
    pub peak: f32,
}
