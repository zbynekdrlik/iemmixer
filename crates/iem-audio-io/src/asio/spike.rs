//! The S1a spike's stream (S1a design note §3): [`Host::start`] creates a
//! buffer on every card channel and starts [`Running`], whose callbacks
//! reach it through the `STREAM` slot and `IN_FLIGHT`. The example
//! `asio_spike` drives it; the S6 engine never does.

use core::ffi::{c_long, c_void};
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use azo::dto::ChannelId;
use azo::sys::{Bool, Callbacks, MessageSelector, SampleRate, Time, TimeInfoFlags};

use super::{AsioError, BUSY, DriverInfo, Host, half, nanos, pump_messages, read, zero};
use crate::format::{self, Refusal, SampleFormat};
use crate::os;
use crate::telemetry::{self, Glitch, InputPeaks, Snapshot, Telemetry};

/// Options of one stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamConfig {
    /// The buffer the owner set in the driver; must equal its preferred size.
    pub frames: i32,
    /// Busy-work per callback standing in for the engine's DSP (µs).
    pub burn_us: u32,
    /// Fault injection: panic inside callback number `panic_at` (0 = never).
    pub panic_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StartTimings {
    pub create_buffers: Duration,
    pub start: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StopTimings {
    pub stop: Duration,
    pub dispose: Duration,
    pub stop_ok: bool,
    pub dispose_ok: bool,
    /// A callback did not leave the stream within [`STOP_WAIT`] (R6): the
    /// stream and the buffers are left alone (leaked, never freed under the
    /// callback) and no further stream starts in this process.
    pub hung: bool,
}

/// How long `finish` waits for the last callback to leave the stream.
pub const STOP_WAIT: Duration = Duration::from_secs(2);

impl Host {
    /// Creates a buffer on every input and output of the card, zeroes the
    /// outputs and starts streaming.
    pub fn start(&self, info: &DriverInfo, cfg: StreamConfig) -> Result<Running<'_>, AsioError> {
        let d = &*self.driver;
        let format = format::admit(
            info.rate,
            info.buffer_preferred,
            cfg.frames,
            &info.sample_types,
        )
        .map_err(AsioError::Refused)?;
        let frames = u32::try_from(cfg.frames).map_err(|_| {
            AsioError::Refused(Refusal::Buffer {
                preferred: info.buffer_preferred,
                expected: cfg.frames,
            })
        })?;
        // One stream per process, claimed atomically; released by `finish`
        // (never after a hung stop) or below when createBuffers fails.
        if BUSY
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(AsioError::Busy);
        }
        let channels: Vec<ChannelId> = (0..info.inputs)
            .map(|index| ChannelId { input: true, index })
            .chain((0..info.outputs).map(|index| ChannelId {
                input: false,
                index,
            }))
            .collect();
        let t = Instant::now();
        // SAFETY: CALLBACKS is a static, so it outlives the buffers. The
        // pointers are dereferenced only by callbacks of this stream, which
        // end before `Running::finish` disposes the buffers.
        let created =
            unsafe { d.create_buffers(channels.iter().copied(), cfg.frames, &raw const CALLBACKS) }
                .map_err(self.call("createBuffers"));
        let buffers: Vec<[*mut c_void; 2]> = match created {
            Ok(b) => b.collect(),
            Err(e) => {
                BUSY.store(false, Ordering::SeqCst);
                return Err(e);
            }
        };
        let create_buffers = t.elapsed();
        let split = usize::try_from(info.inputs).unwrap_or(0).min(buffers.len());
        let (inputs, outputs) = buffers.split_at(split);
        // The stream clock's zero and its QPC count, read back to back.
        let base = Instant::now();
        let (base_qpc, qpc_freq) = os::qpc().unwrap_or((0, 0));
        let stream = Box::new(Stream {
            format,
            bytes: (frames as usize).saturating_mul(format.bytes()),
            inputs: inputs.to_vec(),
            outputs: outputs.to_vec(),
            telemetry: Telemetry::new(frames, info.rate),
            peaks: InputPeaks::new(inputs.len()),
            base,
            base_qpc,
            qpc_freq,
            burn: Duration::from_micros(u64::from(cfg.burn_us)),
            panic_at: cfg.panic_at,
            faulted: AtomicBool::new(false),
            output_ready: AtomicBool::new(true),
            driver: ptr::from_ref(d),
        });
        for ch in &stream.outputs {
            let [a, b] = *ch;
            zero(a, stream.bytes);
            zero(b, stream.bytes);
        }
        let raw = Box::into_raw(stream);
        STREAM.store(raw, Ordering::SeqCst);
        let t = Instant::now();
        let started = d.start();
        let start = t.elapsed();
        let running = Running {
            host: self,
            stream: raw,
            timings: StartTimings {
                create_buffers,
                start,
            },
        };
        match started {
            Ok(()) => Ok(running),
            Err(e) => {
                let err = self.call("start")(e);
                running.finish();
                Err(err)
            }
        }
    }
}

/// A started stream; [`Running::finish`] (or drop) stops it.
pub struct Running<'h> {
    host: &'h Host,
    stream: *mut Stream,
    pub timings: StartTimings,
}

impl Running<'_> {
    fn stream(&self) -> Option<&Stream> {
        // SAFETY: `stream` is either null or the live Box this Running owns.
        unsafe { self.stream.as_ref() }
    }

    pub fn snapshot(&self) -> Option<Snapshot> {
        self.stream().map(|s| s.telemetry.snapshot())
    }

    /// The QPC count at the stream clock's zero and the QPC frequency.
    pub fn qpc_base(&self) -> Option<(i64, i64)> {
        self.stream().map(|s| (s.base_qpc, s.qpc_freq))
    }

    /// Moves the glitches since the last call into `out` (owner thread only).
    pub fn drain_glitches(&self, out: &mut Vec<Glitch>) {
        if let Some(s) = self.stream() {
            s.telemetry.drain_glitches(out);
        }
    }

    pub fn callbacks(&self) -> u64 {
        self.stream().map_or(0, |s| s.telemetry.callbacks())
    }

    pub fn rate_changed(&self) -> bool {
        self.stream()
            .is_some_and(|s| s.telemetry.rate_changes() > 0)
    }

    /// Each input's peak since the last call (index from 0).
    pub fn take_input_peaks(&self) -> Vec<f64> {
        self.stream().map_or_else(Vec::new, |s| s.peaks.take())
    }

    pub fn take_reopen(&self) -> bool {
        self.stream().is_some_and(|s| s.telemetry.take_reopen())
    }

    pub fn faulted(&self) -> bool {
        self.stream()
            .is_some_and(|s| s.faulted.load(Ordering::SeqCst))
    }

    /// Stops the driver, waits until no callback is inside the stream,
    /// disposes the buffers and frees the stream. Returns the last snapshot.
    pub fn finish(mut self) -> (Option<Snapshot>, StopTimings) {
        self.stop_now()
    }

    fn stop_now(&mut self) -> (Option<Snapshot>, StopTimings) {
        let raw = core::mem::replace(&mut self.stream, ptr::null_mut());
        if raw.is_null() {
            return (None, StopTimings::default());
        }
        let d = &*self.host.driver;
        let t = Instant::now();
        let stopped = d.stop();
        let stop = t.elapsed();
        STREAM.store(ptr::null_mut(), Ordering::SeqCst);
        // Bounded, and pumping: a driver may need this thread's messages to
        // finish a callback (R6: a hang is reported, never killed).
        let wait = Instant::now();
        while IN_FLIGHT.load(Ordering::SeqCst) != 0 {
            if wait.elapsed() >= STOP_WAIT {
                // SAFETY: the stream is never freed from here on (leaked), so
                // the callback still inside it keeps a valid reference.
                let snapshot = unsafe { &*raw }.telemetry.snapshot();
                return (
                    Some(snapshot),
                    StopTimings {
                        stop,
                        stop_ok: stopped.is_ok(),
                        hung: true,
                        ..StopTimings::default()
                    },
                );
            }
            pump_messages();
            std::thread::yield_now();
        }
        // SAFETY: the slot no longer points to the stream and no callback is
        // inside it, so this is the only reference; it came from Box::into_raw.
        let stream = unsafe { Box::from_raw(raw) };
        let snapshot = stream.telemetry.snapshot();
        let t = Instant::now();
        let disposed = d.dispose_all_buffers();
        let dispose = t.elapsed();
        drop(stream);
        BUSY.store(false, Ordering::SeqCst);
        (
            Some(snapshot),
            StopTimings {
                stop,
                dispose,
                stop_ok: stopped.is_ok(),
                dispose_ok: disposed.is_ok(),
                hung: false,
            },
        )
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        let _ = self.stop_now();
    }
}

struct Stream {
    format: SampleFormat,
    /// One half-buffer of one channel.
    bytes: usize,
    inputs: Vec<[*mut c_void; 2]>,
    outputs: Vec<[*mut c_void; 2]>,
    telemetry: Telemetry,
    peaks: InputPeaks,
    base: Instant,
    /// The QPC count read right after `base` and the QPC frequency (glitch
    /// times in QPC for the trace markers, S1c design note §4.1).
    base_qpc: i64,
    qpc_freq: i64,
    burn: Duration,
    panic_at: u64,
    faulted: AtomicBool,
    output_ready: AtomicBool,
    driver: *const azo::Driver,
}

static STREAM: AtomicPtr<Stream> = AtomicPtr::new(ptr::null_mut());
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
static CALLBACKS: Callbacks = Callbacks {
    buffer_switch: on_buffer_switch,
    sample_rate_did_change: on_rate_change,
    asio_message: on_message,
    buffer_switch_time_info: on_buffer_switch_time_info,
};

/// Runs `f` on the live stream, if any, keeping it alive meanwhile.
fn with_stream<R>(f: impl FnOnce(&Stream) -> R) -> Option<R> {
    IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
    let raw = STREAM.load(Ordering::SeqCst);
    // SAFETY: a non-null slot points to a live stream: its owner clears the
    // slot and waits for IN_FLIGHT == 0 before it frees the stream, and this
    // callback incremented IN_FLIGHT before it read the slot.
    let out = unsafe { raw.as_ref() }.map(f);
    IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    out
}

unsafe extern "system" fn on_buffer_switch(index: c_long, _direct: Bool) {
    with_stream(|s| s.on_buffer(index != 0, None));
}

unsafe extern "system" fn on_buffer_switch_time_info(
    params: *mut Time,
    index: c_long,
    _direct: Bool,
) -> *mut Time {
    // SAFETY: the driver passes a valid Time for this call, or null.
    // Note for the fork / S6: azo-sys 0.2.1 `I64Split` (inside `#[repr(C)]
    // TimeInfo`, and the out-parameter of getSamplePosition) has no
    // `#[repr(C)]` of its own; it works because rustc keeps two `u32` fields
    // in order. The PC window's position-gap counts would show a break.
    let position = unsafe { params.as_ref() }
        .filter(|t| {
            t.time_info
                .flags
                .contains(TimeInfoFlags::SAMPLE_POSITION_VALID)
        })
        .map(|t| i64::from(t.time_info.sample_position));
    with_stream(|s| s.on_buffer(index != 0, position));
    params
}

unsafe extern "system" fn on_message(
    sel: MessageSelector,
    value: c_long,
    _message: *const c_void,
    _opt: *const f64,
) -> c_long {
    // Drivers ask (supported selectors, engine version, time info) while
    // createBuffers runs, before the stream exists: answer without counting.
    with_stream(|s| s.telemetry.driver_message(sel.0, value))
        .unwrap_or_else(|| telemetry::reply(sel.0, value))
}

unsafe extern "system" fn on_rate_change(_rate: SampleRate) {
    with_stream(|s| s.telemetry.on_rate_change());
}

impl Stream {
    fn on_buffer(&self, second: bool, position: Option<i64>) {
        let entry = self.base.elapsed();
        self.telemetry
            .on_thread(os::current_processor(), os::current_thread_id());
        let position = position.or_else(|| {
            // SAFETY: the driver outlives the stream (Running borrows Host).
            unsafe { self.driver.as_ref() }
                .and_then(|d| d.sample_position().ok())
                .map(|p| p.position)
        });
        self.telemetry.on_callback(nanos(entry).max(1), position);
        for ch in &self.outputs {
            zero(half(ch, second), self.bytes);
        }
        if !self.faulted.load(Ordering::Relaxed) {
            let caught = catch_unwind(AssertUnwindSafe(|| self.work(second)));
            if caught.is_err() {
                self.faulted.store(true, Ordering::SeqCst);
                for ch in &self.outputs {
                    zero(half(ch, second), self.bytes);
                }
            }
        }
        if self.output_ready.load(Ordering::Relaxed) {
            // SAFETY: as above.
            let ok = unsafe { self.driver.as_ref() }.is_some_and(|d| d.output_ready().is_ok());
            if !ok {
                // ASE_NotPresent: the driver does not need the signal.
                self.output_ready.store(false, Ordering::Relaxed);
            }
        }
        self.telemetry
            .on_done(nanos(self.base.elapsed().saturating_sub(entry)));
    }

    fn work(&self, second: bool) {
        let n = self.telemetry.callbacks();
        if self.panic_at != 0 && n == self.panic_at {
            inject_fault(n);
        }
        for (i, ch) in self.inputs.iter().enumerate() {
            self.peaks
                .record(i, self.format.peak(read(half(ch, second), self.bytes)));
        }
        if !self.burn.is_zero() {
            let t = Instant::now();
            while t.elapsed() < self.burn {
                core::hint::spin_loop();
            }
        }
    }
}

/// The default panic hook formats the message and locks stderr inside the
/// callback: acceptable for the spike's `--panic-at`, but S6 installs a
/// hook that neither allocates nor locks on the callback thread.
#[allow(
    clippy::panic,
    reason = "fault injection (program spec §2.4), a dev-only flag"
)]
fn inject_fault(n: u64) -> ! {
    panic!("injected fault at callback {n}")
}
