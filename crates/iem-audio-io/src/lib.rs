//! Audio backends of the iemmixer engine (program spec §2.2; S3 design note
//! §1, §3.7): the [`Process`] trait the engine implements and two backends
//! that need no sound card:
//!
//! - [`Offline`]: deterministic, any block size — the parity harness;
//! - [`NullRt`]: paced real time with synthetic inputs — E2E and soak runs.
//!
//! For the card (S1a design note §3; S6 design note §3): [`format`] (ASIO
//! sample types ↔ f64, the I2 refusals), [`telemetry`] (lock-free callback
//! statistics), [`channels`] (topology card channels → card indices),
//! [`period`] (the period the driver really delivers, measured from sample
//! positions), [`reset`] (the reopen budget and the stall rule), [`rtpanic`]
//! (panics on the real-time thread, recorded in atomics), [`owner`] (the
//! decisions of the backend's owner thread), [`messages`] (the driver's
//! messages, counted on any thread and logged by the owner thread),
//! [`cpuset`] (CPU lists), [`glitch_report`] (the spike's capped glitch list
//! and trace-marker text), [`spike_run`] (the spike's run decisions: argument
//! rule, applied levers, the hwlat gate, exit codes), [`hist`] (the stream
//! histograms of the callback interval and the callback's own time, which
//! NullRt and the ASIO backend record, S7) and, on Windows only, `asio` and
//! `os` — the crate's only unsafe code: the S1a spike host and the S6 backend
//! `AsioStream`. The preference window (the driver's preferred buffer holds
//! 32 while the engine holds the card, REAPER's original at every other
//! moment) lives in `iem_win::prefwin`, which the guard shares.
//!
//! Buffers are f64 and channel-major. Every backend calls `process()` inside
//! `catch_unwind`: a panic zeroes that block's outputs, the processor is never
//! called again and the fault is reported (§2.4 crash model). After a driver
//! reopen the ASIO backend calls [`Process::discontinuity`] before the next
//! block, and [`StreamStats`] carries the measured period, missed periods,
//! overruns, resets, the last reopen's time, a parked stream and the
//! faulting callback's time.

#![deny(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic
    )
)]

#[cfg(windows)]
#[allow(
    unsafe_code,
    reason = "ASIO FFI: azo buffers and callbacks (S1a design note §3)"
)]
pub mod asio;
pub mod channels;
pub mod cpuset;
pub mod format;
pub mod glitch_report;
pub mod messages;
pub mod nullrt;
pub mod offline;
#[cfg(windows)]
#[allow(
    unsafe_code,
    reason = "Windows placement, priority, clock and trace markers (S1c design note §6.2 L5)"
)]
pub mod os;
pub mod owner;
pub mod period;
pub mod reset;
pub mod rtpanic;
pub mod spike_run;
pub mod wav;

// The audio contract the Dante device shares (#72, 2026-10-30): defined once
// in dante-stack's dante-audio-io, so iem-engine's `Process` is the very trait
// the device's pump calls.
pub use dante_audio_io::{
    Block, Fault, Planar, Process, StreamStats, hist, panic_message, telemetry,
};
pub use nullrt::{InputSignal, NullRt, NullRtConfig};
pub use offline::{Offline, OfflineRun};
