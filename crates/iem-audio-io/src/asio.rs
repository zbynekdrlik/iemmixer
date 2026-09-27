//! ASIO host on azo 0.2.1 (S1a spike; the S6 backend grows from it; design
//! note `docs/superpowers/specs/2026-09-27-s1a-asio-spike-design.md` §3).
//!
//! - Every driver call happens on the thread that created the driver (COM
//!   STA); that thread also pumps its window messages. [`Host`] is `!Send`.
//! - ASIO callbacks carry no user pointer: one global slot holds the running
//!   stream, and a counter of callbacks in flight lets the owner free the
//!   stream only after the last callback left it.
//! - Every output channel of the card gets a buffer, zeroed before `start()`
//!   and on every callback, also after a caught panic (A1).
//! - The host never sets the sample rate, never selects a clock source and
//!   never opens the control panel; it refuses any rate but 96 kHz and any
//!   buffer but the driver's preferred one (I2).

use core::ffi::{CStr, c_long, c_void};
use core::fmt;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use azo::dto::{ChannelId, Granularity};
use azo::sys::{Bool, Callbacks, MessageSelector, SampleRate, Time, TimeInfoFlags};
use azo::utils::com::InitGuard;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
};

use crate::format::{self, Refusal, SampleFormat};
use crate::telemetry::{self, Snapshot, Telemetry};

#[derive(Debug)]
pub enum AsioError {
    /// No ASIO driver is registered (no `HKLM\SOFTWARE\ASIO`).
    NoDrivers(String),
    /// No driver has this description; the registered ones are listed.
    NotFound {
        wanted: String,
        present: Vec<String>,
    },
    /// COM could not create the driver object.
    Create(String),
    /// `init()` returned false; the driver's error text.
    Init(String),
    /// An ASIO call failed: which call, and the driver's error text.
    Call(&'static str, String),
    Refused(Refusal),
    /// A stream already runs in this process.
    Busy,
}

impl fmt::Display for AsioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDrivers(e) => write!(f, "no ASIO drivers registered: {e}"),
            Self::NotFound { wanted, present } => {
                write!(f, "no ASIO driver named {wanted:?}; present: {present:?}")
            }
            Self::Create(e) => write!(f, "creating the driver failed: {e}"),
            Self::Init(e) => write!(f, "the driver refused init(): {e}"),
            Self::Call(what, e) => write!(f, "{what} failed: {e}"),
            Self::Refused(r) => write!(f, "refused: {r}"),
            Self::Busy => f.write_str("a stream already runs in this process"),
        }
    }
}

impl std::error::Error for AsioError {}

#[derive(Debug, Clone, PartialEq)]
pub struct ClockInfo {
    pub index: i32,
    pub name: String,
    pub current: bool,
}

/// What the driver reports without any buffer (read-only calls only).
#[derive(Debug, Clone, PartialEq)]
pub struct DriverInfo {
    pub name: String,
    pub version: i32,
    pub inputs: i32,
    pub outputs: i32,
    pub buffer_min: i32,
    pub buffer_max: i32,
    pub buffer_preferred: i32,
    /// `fixed`, `power-of-two` or `linear:<step>`.
    pub buffer_granularity: String,
    pub rate: f64,
    pub can_96k: bool,
    /// At the preferred size, before buffers exist (samples).
    pub latency_in: i32,
    pub latency_out: i32,
    pub clocks: Vec<ClockInfo>,
    /// ASIOSampleType per channel: inputs, then outputs.
    pub sample_types: Vec<i32>,
}

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
}

/// A driver instance, created, used and released on this thread.
pub struct Host {
    driver: InitGuard<azo::Driver>,
}

impl Host {
    /// Creates and initialises the driver whose registry description is `description`.
    pub fn open(description: &str) -> Result<Self, AsioError> {
        let drivers = azo::get_drivers().map_err(|e| AsioError::NoDrivers(e.to_string()))?;
        let Some(meta) = drivers
            .iter()
            .find(|d| d.description.to_string_lossy() == description)
        else {
            return Err(AsioError::NotFound {
                wanted: description.to_owned(),
                present: drivers
                    .iter()
                    .map(|d| d.description.to_string_lossy())
                    .collect(),
            });
        };
        let driver = meta
            .create_instance()
            .map_err(|e| AsioError::Create(e.to_string()))?;
        if !driver.init(None) {
            return Err(AsioError::Init(text(&driver.last_error())));
        }
        Ok(Self { driver })
    }

    fn call(&self, what: &'static str) -> impl Fn(azo::Error) -> AsioError + '_ {
        move |e| AsioError::Call(what, format!("{e} ({})", text(&self.driver.last_error())))
    }

    pub fn info(&self) -> Result<DriverInfo, AsioError> {
        let d = &*self.driver;
        let counts = d.channel_counts().map_err(self.call("getChannels"))?;
        let size = d.buffer_size().map_err(self.call("getBufferSize"))?;
        let rate = d.get_sample_rate().map_err(self.call("getSampleRate"))?;
        let latency = d.latencies().map_err(self.call("getLatencies"))?;
        let clocks = d.clock_sources().map_err(self.call("getClockSources"))?;
        let mut sample_types = Vec::new();
        for (input, n) in [(true, counts.in_), (false, counts.out)] {
            for index in 0..n {
                let info = d
                    .channel_info(ChannelId { input, index })
                    .map_err(self.call("getChannelInfo"))?;
                sample_types.push(info.sample_type.0);
            }
        }
        Ok(DriverInfo {
            name: text(&d.name()),
            version: d.version(),
            inputs: counts.in_,
            outputs: counts.out,
            buffer_min: size.min,
            buffer_max: size.max,
            buffer_preferred: size.preferred,
            buffer_granularity: match size.granularity {
                None => "fixed".to_owned(),
                Some(Granularity::Exponential) => "power-of-two".to_owned(),
                Some(Granularity::Linear { step }) => format!("linear:{step}"),
            },
            rate,
            can_96k: d.can_sample_rate(format::RATE).is_ok(),
            latency_in: latency.in_,
            latency_out: latency.out,
            clocks: clocks
                .iter()
                .map(|c| ClockInfo {
                    index: c.index,
                    name: CStr::from_bytes_until_nul(&c.name)
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    current: c.is_current_source != Bool::FALSE,
                })
                .collect(),
            sample_types,
        })
    }

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
        if !STREAM.load(Ordering::SeqCst).is_null() {
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
        let buffers: Vec<[*mut c_void; 2]> =
            unsafe { d.create_buffers(channels.iter().copied(), cfg.frames, &raw const CALLBACKS) }
                .map_err(self.call("createBuffers"))?
                .collect();
        let create_buffers = t.elapsed();
        let split = usize::try_from(info.inputs).unwrap_or(0).min(buffers.len());
        let (inputs, outputs) = buffers.split_at(split);
        let stream = Box::new(Stream {
            format,
            bytes: (frames as usize).saturating_mul(format.bytes()),
            inputs: inputs.to_vec(),
            outputs: outputs.to_vec(),
            telemetry: Telemetry::new(frames, info.rate),
            base: Instant::now(),
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

    /// Reads the latencies the driver reports now (samples).
    pub fn latencies(&self) -> Result<(i32, i32), AsioError> {
        let l = self.driver.latencies().map_err(self.call("getLatencies"))?;
        Ok((l.in_, l.out))
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

    pub fn callbacks(&self) -> u64 {
        self.stream().map_or(0, |s| s.telemetry.callbacks())
    }

    pub fn rate_changed(&self) -> bool {
        self.stream()
            .is_some_and(|s| s.telemetry.rate_changes() > 0)
    }

    pub fn take_input_peak(&self) -> f64 {
        self.stream().map_or(0.0, |s| s.telemetry.take_input_peak())
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
        while IN_FLIGHT.load(Ordering::SeqCst) != 0 {
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
        (
            Some(snapshot),
            StopTimings {
                stop,
                dispose,
                stop_ok: stopped.is_ok(),
                dispose_ok: disposed.is_ok(),
            },
        )
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        let _ = self.stop_now();
    }
}

/// Dispatches this thread's pending window messages (drivers may post to a
/// hidden window of the thread that created them).
pub fn pump_messages() {
    let mut msg = MSG::default();
    // SAFETY: plain Win32 calls on this thread's own queue with a valid MSG.
    unsafe {
        while PeekMessageW(&mut msg, ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn text(s: &core::ffi::CStr) -> String {
    s.to_string_lossy().into_owned()
}

struct Stream {
    format: SampleFormat,
    /// One half-buffer of one channel.
    bytes: usize,
    inputs: Vec<[*mut c_void; 2]>,
    outputs: Vec<[*mut c_void; 2]>,
    telemetry: Telemetry,
    base: Instant,
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
        let mut peak = 0.0_f64;
        for ch in &self.inputs {
            peak = peak.max(self.format.peak(read(half(ch, second), self.bytes)));
        }
        self.telemetry.on_input_peak(peak);
        if !self.burn.is_zero() {
            let t = Instant::now();
            while t.elapsed() < self.burn {
                core::hint::spin_loop();
            }
        }
    }
}

#[allow(
    clippy::panic,
    reason = "fault injection (program spec §2.4), a dev-only flag"
)]
fn inject_fault(n: u64) -> ! {
    panic!("injected fault at callback {n}")
}

fn half(ch: &[*mut c_void; 2], second: bool) -> *mut c_void {
    let [a, b] = *ch;
    if second { b } else { a }
}

fn zero(p: *mut c_void, bytes: usize) {
    if !p.is_null() {
        // SAFETY: the driver allocated `bytes` bytes behind each half-buffer
        // pointer (buffer size × sample size) for the life of the buffers.
        unsafe { ptr::write_bytes(p.cast::<u8>(), 0, bytes) };
    }
}

fn read<'a>(p: *mut c_void, bytes: usize) -> &'a [u8] {
    if p.is_null() {
        return &[];
    }
    // SAFETY: as in `zero`; the driver does not write this half while the host
    // is inside the callback for it.
    unsafe { core::slice::from_raw_parts(p.cast::<u8>().cast_const(), bytes) }
}

fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}
