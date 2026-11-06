//! Audio backends of the iemmixer engine (program spec §2.2; S3 design note
//! §1, §3.7): the [`Process`] trait the engine implements and two backends
//! that need no sound card:
//!
//! - [`Offline`]: deterministic, any block size — the parity harness;
//! - [`NullRt`]: paced real time with synthetic inputs — E2E and soak runs.
//!
//! Beside them: [`channels`] (topology channels to device channel indices),
//! [`owner`] (how a stream ended) and [`wav`] (the offline renders' files).
//!
//! Buffers are f64 and channel-major. Every backend calls `process()` inside
//! `catch_unwind`: a panic zeroes that block's outputs, the processor is never
//! called again and the fault is reported (§2.4 crash model).

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

pub mod channels;
pub mod nullrt;
pub mod offline;
pub mod owner;
pub mod wav;

// The audio contract the Dante device shares (#72, 2026-10-30): defined once
// in dstack's dstack-audio-io, so iem-engine's `Process` is the very trait
// the device's pump calls.
pub use dstack_audio_io::{
    Block, Fault, Planar, Process, StreamStats, hist, panic_message, telemetry,
};
pub use nullrt::{InputSignal, NullRt, NullRtConfig};
pub use offline::{Offline, OfflineRun};
