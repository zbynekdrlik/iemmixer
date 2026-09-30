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
//! and trace-marker text) and, on Windows only, `asio` and `os` — the crate's
//! only unsafe code: the S1a spike host and the S6 backend `AsioStream`. The preference window (the driver's
//! preferred buffer holds 32 while the engine holds the card, REAPER's
//! original at every other moment) lives in `iem_win::prefwin`, which the
//! guard shares.
//!
//! Buffers are f64 and channel-major. Every backend calls `process()` inside
//! `catch_unwind`: a panic zeroes that block's outputs, the processor is never
//! called again and the fault is reported (§2.4 crash model). After a driver
//! reopen the ASIO backend calls [`Process::discontinuity`] before the next
//! block, and [`StreamStats`] carries the measured period, missed periods,
//! overruns, resets and a parked stream.

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

use core::any::Any;
use core::ops::Range;

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
pub mod telemetry;
pub mod wav;

pub use nullrt::{InputSignal, NullRt, NullRtConfig};
pub use offline::{Offline, OfflineRun};

fn span(ch: usize, frames: usize) -> Option<Range<usize>> {
    let start = ch.checked_mul(frames)?;
    Some(start..start.checked_add(frames)?)
}

/// One callback's audio: `frames` samples per channel, channel-major.
#[derive(Debug)]
pub struct Block<'a> {
    frames: usize,
    input: &'a [f64],
    output: &'a mut [f64],
}

impl<'a> Block<'a> {
    /// `input` and `output` hold whole channels of `frames` samples each.
    pub fn new(frames: usize, input: &'a [f64], output: &'a mut [f64]) -> Self {
        Self {
            frames,
            input,
            output,
        }
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    pub fn inputs(&self) -> usize {
        self.input.len().checked_div(self.frames).unwrap_or(0)
    }

    pub fn outputs(&self) -> usize {
        self.output.len().checked_div(self.frames).unwrap_or(0)
    }

    /// Input channel `ch`; empty when there is no such channel.
    pub fn input(&self, ch: usize) -> &[f64] {
        span(ch, self.frames)
            .and_then(|r| self.input.get(r))
            .unwrap_or_default()
    }

    /// Output channel `ch`; empty when there is no such channel.
    pub fn output(&mut self, ch: usize) -> &mut [f64] {
        span(ch, self.frames)
            .and_then(|r| self.output.get_mut(r))
            .unwrap_or_default()
    }

    pub fn zero_outputs(&mut self) {
        self.output.fill(0.0);
    }
}

/// The engine's audio callback. `process` must not allocate, lock, make
/// syscalls or log (I7).
pub trait Process: Send {
    fn process(&mut self, block: &mut Block<'_>);

    /// Called on the callback thread before the first block after a reopen;
    /// the engine restarts its fade-in. The same rules as `process` (I7).
    fn discontinuity(&mut self) {}
}

/// A running stream's statistics, read by the control thread.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamStats {
    /// The period the driver really delivers, in frames per callback: the
    /// ASIO backend measures it from the first callbacks' sample positions
    /// ([`period`]); NullRt reports its block size.
    pub frames: u32,
    pub callbacks: u64,
    /// Late callbacks: NullRt counts those finished more than one period
    /// after their deadline, the ASIO backend callback intervals over 1.5 and
    /// under 2 periods (`telemetry::classify`).
    pub late: u64,
    /// Callback intervals of 2 periods or more: a period passed without a
    /// callback (ASIO; NullRt 0).
    pub missed: u64,
    /// Callbacks that took longer than one period (ASIO; NullRt 0).
    pub overruns: u64,
    /// Driver reopens after a reset request or a stall (ASIO; NullRt 0).
    pub resets: u64,
    /// A callback was still in flight after the stop wait: the stream is
    /// left allocated, never freed under a callback, and the guard alarms
    /// (ASIO; NullRt false).
    pub parked: bool,
    pub faulted: bool,
    pub running: bool,
    pub max_process_ns: u64,
    pub fault: Option<String>,
}

/// Multichannel audio, channel-major.
#[derive(Debug, Clone, PartialEq)]
pub struct Planar {
    channels: usize,
    frames: usize,
    data: Vec<f64>,
}

impl Planar {
    pub fn new(channels: usize, frames: usize) -> Self {
        Self {
            channels,
            frames,
            data: vec![0.0; channels.saturating_mul(frames)],
        }
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    pub fn channel(&self, ch: usize) -> &[f64] {
        span(ch, self.frames)
            .and_then(|r| self.data.get(r))
            .unwrap_or_default()
    }

    pub fn channel_mut(&mut self, ch: usize) -> &mut [f64] {
        span(ch, self.frames)
            .and_then(|r| self.data.get_mut(r))
            .unwrap_or_default()
    }
}

/// A panic caught at the backend boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fault {
    /// First frame of the block that panicked.
    pub frame: u64,
    pub message: String,
}

/// The text of a panic payload.
pub fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic with a non-text payload".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_accessors_never_panic() {
        let input = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let mut output = [0.0; 4];
        let mut b = Block::new(2, &input, &mut output);
        assert_eq!((b.frames(), b.inputs(), b.outputs()), (2, 3, 2));
        assert_eq!(b.input(1), &[3.0, 4.0]);
        assert!(b.input(3).is_empty());
        assert!(b.input(usize::MAX).is_empty());
        b.output(1).copy_from_slice(&[7.0, 8.0]);
        assert!(b.output(2).is_empty());
        assert!(b.output(usize::MAX).is_empty());
        assert_eq!(b.output(1), &[7.0, 8.0]);
        b.zero_outputs();
        assert_eq!(b.output(1), &[0.0, 0.0]);
        let mut none: [f64; 0] = [];
        let empty = Block::new(0, &[], &mut none);
        assert_eq!((empty.inputs(), empty.outputs()), (0, 0));
        assert!(empty.input(0).is_empty());
    }

    #[test]
    fn planar_channels() {
        let mut p = Planar::new(2, 3);
        assert_eq!((p.channels(), p.frames()), (2, 3));
        p.channel_mut(1).copy_from_slice(&[1.0, 2.0, 3.0]);
        assert_eq!(p.channel(0), &[0.0; 3]);
        assert_eq!(p.channel(1), &[1.0, 2.0, 3.0]);
        assert!(p.channel(2).is_empty());
        assert!(p.channel_mut(2).is_empty());
    }

    #[test]
    fn panic_messages_are_extracted() {
        let s: Box<dyn Any + Send> = Box::new("static");
        assert_eq!(panic_message(&*s), "static");
        let s: Box<dyn Any + Send> = Box::new(String::from("owned"));
        assert_eq!(panic_message(&*s), "owned");
        let s: Box<dyn Any + Send> = Box::new(7u8);
        assert_eq!(panic_message(&*s), "panic with a non-text payload");
    }
}
