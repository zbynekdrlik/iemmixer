//! ASIO on azo 0.2.1: the S1a spike host ([`Host`], [`Running`]; design note
//! `docs/superpowers/specs/2026-09-27-s1a-asio-spike-design.md` §3) and the
//! S6 backend [`AsioStream`] beside it (S6 design note §3).
//!
//! - Every driver call happens on the thread that created the driver (COM
//!   STA); that thread also pumps its window messages. [`Host`] is `!Send`;
//!   the backend runs one owner thread that makes every driver call.
//! - ASIO callbacks carry no user pointer: one global slot holds the running
//!   stream, and a counter of callbacks in flight lets the owner free the
//!   stream only after the last callback left it.
//! - Every output channel of the card gets a buffer, zeroed before `start()`
//!   and on every callback, also after a caught panic (A1).
//! - The host never sets the sample rate, never selects a clock source and
//!   never opens the control panel; it refuses any rate but 96 kHz and any
//!   buffer but the driver's preferred one (I2).
//! - The backend's callback allocates, locks, logs and makes syscalls never
//!   (I7), apart from the driver's own `getSamplePosition` and `outputReady`;
//!   its decisions live in the portable, mutation-tested `format`,
//!   `telemetry`, `channels`, `period`, `reset`, `owner` and
//!   `iem_win::prefwin`.

use core::cell::{Cell, RefCell, UnsafeCell};
use core::ffi::{CStr, c_long, c_void};
use core::fmt;
use core::marker::PhantomData;
use core::ptr;
use core::sync::atomic::{
    AtomicBool, AtomicI64, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering,
};
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use azo::dto::{ChannelId, Granularity};
use azo::sys::{Bool, Callbacks, MessageSelector, SampleRate, Time, TimeInfoFlags};
use azo::utils::com::InitGuard;
// Re-exported: `CardConfig::pref` and the preference errors name them.
pub use iem_win::prefwin::{self, Pref, PrefError};
use iem_win::registry::HkcuPref;
use iem_win::window::SessionEndWindow;
use windows_sys::Win32::System::Diagnostics::Debug::{
    EXCEPTION_CONTINUE_SEARCH, EXCEPTION_POINTERS, SetUnhandledExceptionFilter,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
};

use crate::channels::{ChannelMap, MapError};
use crate::format::{self, Refusal, SampleFormat};
pub use crate::owner::StopOutcome;
use crate::owner::{self, OpenPeriod, SehStep, Watchdog};
use crate::period::PeriodVerdict;
use crate::reset::{ResetBudget, Verdict};
use crate::rtpanic;
use crate::telemetry::{self, Counters, InputPeaks, Snapshot, Telemetry};
use crate::{Block, Process, StreamStats};

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
    // The S6 backend ([`AsioStream`]). The card refusals (exit 3, S6 design
    // note §4) are `NoDrivers`, `NotFound`, `Refused`, `Frames`, `Held`,
    // `Scan`, `Channels`, `Pref`, `PrefLeave` and `Period`.
    /// The configured buffer is not a positive sample count (checked before
    /// the preference window writes anything).
    Frames(i32),
    /// Processes that have the driver module loaded (I3): pid and image name.
    Held(Vec<(u32, String)>),
    /// The driver module's holders could not be listed.
    Scan(String),
    /// The topology's card channels are not on the card.
    Channels(MapError),
    /// The preference window did not open (nothing was written, or the
    /// write read back wrong).
    Pref(PrefError),
    /// The preference window did not close: the driver's preferred buffer
    /// may not hold REAPER's original (the guard's restore follows). `after`
    /// is the open's own failure when the open failed too.
    PrefLeave {
        error: PrefError,
        after: Option<Box<AsioError>>,
    },
    /// The period the driver delivers, measured from the first callbacks,
    /// is not the configured one, or could not be decided.
    Period(PeriodVerdict),
    /// The stream's owner thread did not start or ended without an answer.
    Thread(String),
    /// The Windows session ended while the card opened: the open stopped
    /// and the driver was released.
    SessionEnd,
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
            Self::Frames(n) => write!(
                f,
                "the configured buffer of {n} samples is not a positive size"
            ),
            Self::Held(holders) => {
                let list: Vec<String> = holders
                    .iter()
                    .map(|(pid, image)| format!("{image} (pid {pid})"))
                    .collect();
                write!(f, "the driver module is loaded by {} (I3)", list.join(", "))
            }
            Self::Scan(e) => write!(f, "listing the driver module's holders failed: {e}"),
            Self::Channels(e) => write!(f, "the topology does not fit the card: {e}"),
            Self::Pref(e) => write!(f, "{e}"),
            Self::PrefLeave { error, after: None } => {
                write!(f, "the preferred buffer was not restored: {error}")
            }
            Self::PrefLeave {
                error,
                after: Some(open),
            } => write!(
                f,
                "the preferred buffer was not restored: {error} (after the open failed: {open})"
            ),
            Self::Period(PeriodVerdict::Wrong { expected, measured }) => write!(
                f,
                "the driver delivers {measured} samples per callback, expected {expected}"
            ),
            Self::Period(PeriodVerdict::Ok(n)) => {
                write!(f, "the driver delivers {n} samples per callback")
            }
            Self::Period(PeriodVerdict::Undecided) => {
                f.write_str("the driver's period could not be measured from its first callbacks")
            }
            Self::Thread(e) => write!(f, "the stream's owner thread: {e}"),
            Self::SessionEnd => f.write_str("the Windows session ended while the card opened"),
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
    /// A callback did not leave the stream within [`STOP_WAIT`] (R6): the
    /// stream and the buffers are left alone (leaked, never freed under the
    /// callback) and no further stream starts in this process.
    pub hung: bool,
}

/// How long `finish` waits for the last callback to leave the stream.
pub const STOP_WAIT: Duration = Duration::from_secs(2);

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
        let stream = Box::new(Stream {
            format,
            bytes: (frames as usize).saturating_mul(format.bytes()),
            inputs: inputs.to_vec(),
            outputs: outputs.to_vec(),
            telemetry: Telemetry::new(frames, info.rate),
            peaks: InputPeaks::new(inputs.len()),
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
    peaks: InputPeaks,
    base: Instant,
    burn: Duration,
    panic_at: u64,
    faulted: AtomicBool,
    output_ready: AtomicBool,
    driver: *const azo::Driver,
}

static STREAM: AtomicPtr<Stream> = AtomicPtr::new(ptr::null_mut());
/// A stream is claimed (from `start` until `finish` freed it).
static BUSY: AtomicBool = AtomicBool::new(false);
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

// ---------------------------------------------------------------------------
// The S6 backend (S6 design note §3; plan Task 4)
// ---------------------------------------------------------------------------

/// The card as the site's `[card]` table describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardConfig {
    /// The driver's registry description.
    pub driver: String,
    /// The driver DLL: a process holding it refuses the first open (I3).
    pub module: String,
    /// 32 (I2); the measured period must match.
    pub frames: i32,
    /// The driver's preferred-buffer value: HKCU key, value name and
    /// REAPER's original. `None` only in tests.
    pub pref: Option<(String, String, Pref)>,
}

/// The owner thread's pause between two ticks.
const TICK: Duration = Duration::from_millis(5);
/// `AsioStream::start` waits this long for the first open (module scan,
/// driver open, buffers and the period take about 2 s at most).
const OPEN_BOUND: Duration = Duration::from_secs(15);
/// The text Windows shows while the session end waits for the release.
const SESSION_END_REASON: &str = "iemmixer is stopping the audio driver";
/// A callback without a sample position, in the period ring.
const NO_POSITION: i64 = i64::MIN;

/// The backend's open stream; its callbacks reach it through this slot.
static BACKEND: AtomicPtr<Backend> = AtomicPtr::new(ptr::null_mut());
static BACKEND_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
static BACKEND_CALLBACKS: Callbacks = Callbacks {
    buffer_switch: backend_buffer_switch,
    sample_rate_did_change: backend_rate_change,
    asio_message: backend_message,
    buffer_switch_time_info: backend_buffer_switch_time_info,
};
/// No driver instance of the backend exists: cleared just before an open
/// creates one, set once it is dropped (after its buffers were disposed).
/// The SEH filter waits for it; a parked stream never sets it.
static RELEASED: AtomicBool = AtomicBool::new(true);
/// A structured exception reached the filter: the owner thread releases the
/// driver.
static SEH: AtomicBool = AtomicBool::new(false);
/// The filter parked a faulting thread (the driver was not released in time).
static SEH_PARKED: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// How deep this thread is inside the backend's callbacks: the SEH filter
    /// takes a faulting callback's count out of `BACKEND_IN_FLIGHT`.
    static DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// Runs `f` on the backend's live stream, if any, keeping it alive meanwhile.
fn with_backend<R>(f: impl FnOnce(&Backend) -> R) -> Option<R> {
    BACKEND_IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
    DEPTH.with(|d| d.set(d.get() + 1));
    let raw = BACKEND.load(Ordering::SeqCst);
    // SAFETY: a non-null slot points to a live stream: the owner thread
    // clears the slot and waits for BACKEND_IN_FLIGHT == 0 before it frees
    // the stream, and this callback counted itself before it read the slot.
    let out = unsafe { raw.as_ref() }.map(f);
    DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    BACKEND_IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    out
}

unsafe extern "system" fn backend_buffer_switch(index: c_long, _direct: Bool) {
    with_backend(|b| b.on_buffer(index != 0, None));
}

unsafe extern "system" fn backend_buffer_switch_time_info(
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
    with_backend(|b| b.on_buffer(index != 0, position));
    params
}

unsafe extern "system" fn backend_message(
    sel: MessageSelector,
    value: c_long,
    _message: *const c_void,
    _opt: *const f64,
) -> c_long {
    // Asked while createBuffers runs, before the stream exists: answered
    // without counting.
    with_backend(|b| b.telemetry.driver_message(sel.0, value))
        .unwrap_or_else(|| telemetry::reply(sel.0, value))
}

unsafe extern "system" fn backend_rate_change(_rate: SampleRate) {
    with_backend(|b| b.telemetry.on_rate_change());
}

/// What a reopen carries from one stream to the next: the processor and its
/// preallocated buffers (the same pages, so a `VirtualLock` holds).
struct Carry {
    processor: Box<dyn Process>,
    /// Engine inputs, channel-major, in `Topology::rx` order.
    inbuf: Vec<f64>,
    /// Engine outputs, channel-major: `Topology::tx` order, then HIL's spare
    /// outputs.
    outbuf: Vec<f64>,
}

/// One open stream of the backend.
struct Backend {
    format: SampleFormat,
    frames: usize,
    /// One half-buffer of one channel.
    bytes: usize,
    /// Every card input and output.
    inputs: Vec<[*mut c_void; 2]>,
    outputs: Vec<[*mut c_void; 2]>,
    /// The card input of each engine input, the card output of each engine
    /// output ([`ChannelMap`]).
    rx: Vec<usize>,
    tx: Vec<usize>,
    /// Touched by the callback while the stream runs (ASIO callbacks never
    /// overlap) and by the owner thread only once no callback is inside the
    /// stream (`BACKEND` cleared, `BACKEND_IN_FLIGHT == 0`).
    carry: UnsafeCell<Carry>,
    telemetry: Telemetry,
    base: Instant,
    /// The sample positions of the first callbacks (the open's period).
    ring: [AtomicI64; owner::RING],
    ring_len: AtomicUsize,
    /// Set at a reopen: the callback calls `Process::discontinuity` first.
    discontinuity: AtomicBool,
    /// A panic in the processor (or the owner's fault): the outputs stay
    /// zero and the processor is never called again.
    faulted: AtomicBool,
    output_ready: AtomicBool,
    driver: *const azo::Driver,
}

impl Backend {
    fn on_buffer(&self, second: bool, position: Option<i64>) {
        // Every entry: after a reopen the driver may call back on a new thread.
        rtpanic::mark_rt_thread();
        let entry = self.base.elapsed();
        let position = position.or_else(|| {
            // SAFETY: the driver outlives the stream: the owner thread frees
            // the stream before it drops the host.
            unsafe { self.driver.as_ref() }
                .and_then(|d| d.sample_position().ok())
                .map(|p| p.position)
        });
        self.telemetry.on_callback(nanos(entry).max(1), position);
        self.record(position);
        for ch in &self.outputs {
            zero(half(ch, second), self.bytes);
        }
        if !self.faulted.load(Ordering::Acquire) {
            self.render(second);
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

    /// The first callbacks' positions, for the owner thread (single writer:
    /// callbacks never overlap).
    fn record(&self, position: Option<i64>) {
        let n = self.ring_len.load(Ordering::Relaxed);
        if let Some(slot) = self.ring.get(n) {
            slot.store(position.unwrap_or(NO_POSITION), Ordering::Relaxed);
            self.ring_len.store(n + 1, Ordering::Release);
        }
    }

    /// Decode the engine's inputs, process, encode its outputs (the card's
    /// outputs are already zero).
    fn render(&self, second: bool) {
        // SAFETY: see `carry`: only this callback touches it now.
        let Carry {
            processor,
            inbuf,
            outbuf,
        } = unsafe { &mut *self.carry.get() };
        for (dst, &card) in inbuf.chunks_exact_mut(self.frames).zip(&self.rx) {
            match self.inputs.get(card) {
                Some(ch) => {
                    self.format.decode(read(half(ch, second), self.bytes), dst);
                }
                None => dst.fill(0.0),
            }
        }
        outbuf.fill(0.0);
        let discontinuity = self.discontinuity.swap(false, Ordering::AcqRel);
        let caught = catch_unwind(AssertUnwindSafe(|| {
            if discontinuity {
                processor.discontinuity();
            }
            let mut block = Block::new(self.frames, inbuf.as_slice(), outbuf.as_mut_slice());
            processor.process(&mut block);
        }));
        match caught {
            Ok(()) => {
                for (src, &card) in outbuf.chunks_exact(self.frames).zip(&self.tx) {
                    if let Some(ch) = self.outputs.get(card) {
                        self.format
                            .encode(src, writable(half(ch, second), self.bytes));
                    }
                }
            }
            Err(payload) => {
                // The outputs stay zero. The panic hook recorded the place in
                // atomics; the payload is never freed on this thread.
                self.faulted.store(true, Ordering::Release);
                core::mem::forget(payload);
            }
        }
    }

    /// The positions recorded so far, for the owner thread.
    fn positions(&self) -> Vec<Option<i64>> {
        let n = self.ring_len.load(Ordering::Acquire);
        self.ring
            .iter()
            .take(n)
            .map(|p| Some(p.load(Ordering::Relaxed)).filter(|v| *v != NO_POSITION))
            .collect()
    }
}

fn writable<'a>(p: *mut c_void, bytes: usize) -> &'a mut [u8] {
    if p.is_null() {
        return Default::default();
    }
    // SAFETY: as in `zero`; the driver does not touch this half while the
    // host is inside the callback for it, and no other reference to it lives.
    unsafe { core::slice::from_raw_parts_mut(p.cast::<u8>(), bytes) }
}

fn count(n: i32) -> usize {
    usize::try_from(n).unwrap_or(0)
}

/// `(address, bytes)` of a preallocated buffer, for `VirtualLock`.
fn range(buf: &[f64]) -> (usize, usize) {
    (buf.as_ptr().expose_provenance(), size_of_val(buf))
}

/// Clears `RELEASED` while it lives; dropping it sets `RELEASED`.
struct ReleaseMark;

impl ReleaseMark {
    fn clear() -> Self {
        RELEASED.store(false, Ordering::SeqCst);
        Self
    }
}

impl Drop for ReleaseMark {
    fn drop(&mut self) {
        RELEASED.store(true, Ordering::SeqCst);
    }
}

/// The backend's driver instance. The host is boxed, so the callbacks'
/// driver pointer stays valid when the card moves; it is dropped (released)
/// before the mark sets `RELEASED`.
struct Card {
    host: Box<Host>,
    _released: ReleaseMark,
}

impl Card {
    fn open(driver: &str) -> Result<Self, AsioError> {
        let mark = ReleaseMark::clear();
        let host = Box::new(Host::open(driver)?);
        Ok(Self {
            host,
            _released: mark,
        })
    }

    fn driver(&self) -> &azo::Driver {
        &self.host.driver
    }
}

/// The preference window (S6 design note §3): open while the driver opens,
/// closed right after `createBuffers` on every path; dropping it closes it
/// too (a panicking driver call).
struct PrefWindow {
    store: HkcuPref,
    original: Pref,
    open: bool,
    /// Where a close that failed in `Drop` is noted.
    shared: Arc<Shared>,
}

impl PrefWindow {
    fn enter(
        (key, name, original): &(String, String, Pref),
        frames: u32,
        shared: &Arc<Shared>,
    ) -> Result<Self, PrefError> {
        let mut store = HkcuPref {
            key: key.clone(),
            name: name.clone(),
        };
        prefwin::enter(&mut store, original, frames)?;
        Ok(Self {
            store,
            original: original.clone(),
            open: true,
            shared: Arc::clone(shared),
        })
    }

    fn close(mut self) -> Result<(), PrefError> {
        self.open = false;
        prefwin::leave(&mut self.store, &self.original)
    }
}

impl Drop for PrefWindow {
    fn drop(&mut self) {
        // Still open only while a driver call unwinds. A failed close is
        // noted where the engine finds it: `pref_failure` on a live stream
        // (Alarm{pref}, exit 3), `start`'s refusal when the first open died.
        if self.open
            && let Err(error) = prefwin::leave(&mut self.store, &self.original)
        {
            let text = AsioError::PrefLeave {
                error: error.clone(),
                after: None,
            }
            .to_string();
            note(&self.shared.pref_failure, text);
            note(&self.shared.pref_leave, error);
        }
    }
}

/// A driver with buffers, before `start()`.
struct Prepared {
    card: Card,
    rate: f64,
    /// The card's input count (the buffers list the inputs first).
    inputs: usize,
    map: ChannelMap,
    format: SampleFormat,
    buffers: Vec<[*mut c_void; 2]>,
}

impl Prepared {
    fn release(self) {
        let _ = self.card.driver().dispose_all_buffers();
    }
}

/// The started stream, owned by the owner thread.
struct Live {
    card: Card,
    backend: *mut Backend,
    watchdog: Watchdog,
    /// The driver's rate changes seen so far (a new one asks for a reopen).
    rate_changes: u64,
}

/// Shared between the owner thread and the stream's handle.
#[derive(Default)]
struct Shared {
    stop: AtomicBool,
    reopen: AtomicBool,
    session_end: Arc<AtomicBool>,
    outcome: Mutex<Option<StopOutcome>>,
    running: AtomicBool,
    faulted: AtomicBool,
    parked: AtomicBool,
    frames: AtomicU32,
    callbacks: AtomicU64,
    late: AtomicU64,
    missed: AtomicU64,
    overruns: AtomicU64,
    max_ns: AtomicU64,
    resets: AtomicU64,
    fault: Mutex<Option<String>>,
    pref_failure: Mutex<Option<String>>,
    /// The close that a preference window's `Drop` could not do (a driver
    /// call panicked while it was open).
    pref_leave: Mutex<Option<PrefError>>,
    /// The session-end handler ran while the owner thread was inside an
    /// open or a finish, so it could not reach the owner: the owner stops
    /// that open and releases the driver for good as soon as it returns.
    release_pending: AtomicBool,
    /// `(address, bytes)` of the preallocated buffers.
    ranges: Mutex<Vec<(usize, usize)>>,
}

impl Shared {
    fn outcome(&self) -> Option<StopOutcome> {
        self.outcome.lock().ok().and_then(|o| *o)
    }

    fn set_outcome(&self, outcome: StopOutcome) {
        if let Ok(mut o) = self.outcome.lock() {
            *o = Some(outcome);
        }
    }
}

/// Keeps the first value written to `slot`.
fn note<T>(slot: &Mutex<Option<T>>, value: T) {
    if let Ok(mut s) = slot.lock()
        && s.is_none()
    {
        *s = Some(value);
    }
}

fn panic_text() -> String {
    match rtpanic::latest() {
        Some(p) => format!(
            "panic in the audio callback at {}:{}:{}",
            p.file, p.line, p.col
        ),
        None => "panic in the audio callback".to_owned(),
    }
}

/// I3: a process with the driver module loaded refuses the first open (no
/// process names: whoever holds the card).
fn holders(module: &str) -> Result<(), AsioError> {
    let holders =
        iem_win::process::module_holders(module).map_err(|e| AsioError::Scan(e.to_string()))?;
    if holders.is_empty() {
        Ok(())
    } else {
        Err(AsioError::Held(holders))
    }
}

enum Next {
    Continue,
    Exit,
    Park,
}

/// The owner thread's state: the only code that calls the driver.
struct Owner {
    card: CardConfig,
    frames: u32,
    rx: Vec<u16>,
    tx: Vec<u16>,
    shared: Arc<Shared>,
    budget: ResetBudget,
    /// The counters of the streams already closed.
    base: Counters,
    live: Option<Live>,
    /// The processor and its buffers while no stream is open.
    carry: Option<Carry>,
    /// Finished for good: by a stop, the session end or a structured
    /// exception, or parked.
    done: Option<StopOutcome>,
}

impl Owner {
    /// Opens the card and starts a stream with `carry`. On failure the carry
    /// is back in `self.carry` (none when a stuck callback parked it).
    fn open(&mut self, carry: Carry, first: bool) -> Result<(), AsioError> {
        // I3, before the preference window and never after our own open.
        if first && let Err(e) = holders(&self.card.module) {
            self.carry = Some(carry);
            return Err(e);
        }
        match self.prepare() {
            Ok(prepared) => self.start_stream(prepared, carry, first),
            Err(e) => {
                self.carry = Some(carry);
                Err(e)
            }
        }
    }

    /// The preference window around the driver's open and `createBuffers`.
    fn prepare(&self) -> Result<Prepared, AsioError> {
        let window = self
            .card
            .pref
            .as_ref()
            .map(|pref| PrefWindow::enter(pref, self.frames, &self.shared))
            .transpose()
            .map_err(AsioError::Pref)?;
        let prepared = self.prepare_card();
        let left = window.map_or(Ok(()), PrefWindow::close);
        match (prepared, left) {
            (Ok(p), Ok(())) => Ok(p),
            (Ok(p), Err(error)) => {
                p.release();
                Err(AsioError::PrefLeave { error, after: None })
            }
            (Err(e), Ok(())) => Err(e),
            (Err(e), Err(error)) => Err(AsioError::PrefLeave {
                error,
                after: Some(Box::new(e)),
            }),
        }
    }

    fn prepare_card(&self) -> Result<Prepared, AsioError> {
        let card = Card::open(&self.card.driver)?;
        let info = card.host.info()?;
        let inputs = count(info.inputs);
        let map = ChannelMap::new(&self.rx, &self.tx, inputs, count(info.outputs))
            .map_err(AsioError::Channels)?;
        let format = format::admit(
            info.rate,
            info.buffer_preferred,
            self.card.frames,
            &info.sample_types,
        )
        .map_err(AsioError::Refused)?;
        let channels: Vec<ChannelId> = (0..info.inputs)
            .map(|index| ChannelId { input: true, index })
            .chain((0..info.outputs).map(|index| ChannelId {
                input: false,
                index,
            }))
            .collect();
        // SAFETY: BACKEND_CALLBACKS is a static, so it outlives the buffers.
        // The pointers are dereferenced only by callbacks of this stream,
        // which end before `finish` disposes the buffers.
        let created = unsafe {
            card.driver().create_buffers(
                channels.iter().copied(),
                self.card.frames,
                &raw const BACKEND_CALLBACKS,
            )
        }
        .map_err(card.host.call("createBuffers"))?;
        let buffers: Vec<[*mut c_void; 2]> = created.collect();
        Ok(Prepared {
            card,
            rate: info.rate,
            inputs,
            map,
            format,
            buffers,
        })
    }

    /// Zeroed outputs, `start()`, then the period from the first callbacks.
    fn start_stream(
        &mut self,
        prepared: Prepared,
        mut carry: Carry,
        first: bool,
    ) -> Result<(), AsioError> {
        let Prepared {
            card,
            rate,
            inputs,
            map,
            format,
            buffers,
        } = prepared;
        let frames = self.frames as usize;
        // The same sizes at every reopen: no reallocation, the pages stay.
        carry.inbuf.clear();
        carry
            .inbuf
            .resize(map.rx().len().saturating_mul(frames), 0.0);
        carry.outbuf.clear();
        carry
            .outbuf
            .resize(map.tx().len().saturating_mul(frames), 0.0);
        let ranges = vec![range(&carry.inbuf), range(&carry.outbuf)];
        let split = inputs.min(buffers.len());
        let (card_inputs, card_outputs) = buffers.split_at(split);
        let backend = Box::new(Backend {
            format,
            frames,
            bytes: frames.saturating_mul(format.bytes()),
            inputs: card_inputs.to_vec(),
            outputs: card_outputs.to_vec(),
            rx: map.rx().to_vec(),
            tx: map.tx().to_vec(),
            carry: UnsafeCell::new(carry),
            telemetry: Telemetry::new(self.frames, rate),
            base: Instant::now(),
            ring: [const { AtomicI64::new(NO_POSITION) }; owner::RING],
            ring_len: AtomicUsize::new(0),
            discontinuity: AtomicBool::new(!first),
            faulted: AtomicBool::new(false),
            output_ready: AtomicBool::new(true),
            driver: ptr::from_ref(card.driver()),
        });
        for ch in &backend.outputs {
            let [a, b] = *ch;
            zero(a, backend.bytes);
            zero(b, backend.bytes);
        }
        let raw = Box::into_raw(backend);
        BACKEND.store(raw, Ordering::SeqCst);
        let mut live = Live {
            card,
            backend: raw,
            watchdog: Watchdog::new(Instant::now()),
            rate_changes: 0,
        };
        if let Err(e) = live.card.driver().start() {
            let err = live.card.host.call("start")(e);
            self.carry = self.finish(live);
            return Err(err);
        }
        let started = Instant::now();
        loop {
            // A driver may need this thread's messages to call back.
            pump_messages();
            if self.shared.release_pending.load(Ordering::SeqCst) {
                // The session ended meanwhile (`session_end`): no stream.
                self.carry = self.finish(live);
                return Err(AsioError::SessionEnd);
            }
            // SAFETY: the stream is live: only `finish` frees it.
            let ring = unsafe { &*raw }.positions();
            match owner::open_period(&ring, self.frames, started.elapsed()) {
                OpenPeriod::Wait => thread::sleep(Duration::from_millis(1)),
                OpenPeriod::Ok(n) => {
                    self.shared.frames.store(n, Ordering::SeqCst);
                    break;
                }
                OpenPeriod::Refuse(verdict) => {
                    self.carry = self.finish(live);
                    return Err(AsioError::Period(verdict));
                }
            }
        }
        if let Ok(mut r) = self.shared.ranges.lock() {
            *r = ranges;
        }
        live.watchdog = Watchdog::new(Instant::now());
        self.live = Some(live);
        self.shared.running.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Stops the stream, waits until no callback is inside it (bounded by
    /// [`STOP_WAIT`], pumping), disposes the buffers and releases the driver
    /// (`RELEASED` is set after that); returns the carry. `None` when a
    /// callback stayed inside (R6): the stream and the driver are left alone,
    /// never freed under a callback, and the owner is done (parked).
    fn finish(&mut self, live: Live) -> Option<Carry> {
        let Live { card, backend, .. } = live;
        let _ = card.driver().stop();
        BACKEND.store(ptr::null_mut(), Ordering::SeqCst);
        self.shared.running.store(false, Ordering::SeqCst);
        let wait = Instant::now();
        while BACKEND_IN_FLIGHT.load(Ordering::SeqCst) != 0 {
            if wait.elapsed() >= STOP_WAIT {
                // SAFETY: a parked stream is never freed, so its counters
                // stay readable; `stats` keeps counting its callbacks.
                self.base = self.base.plus(unsafe { &*backend }.telemetry.counters());
                core::mem::forget(card);
                self.shared.parked.store(true, Ordering::SeqCst);
                self.done = Some(StopOutcome::Parked);
                return None;
            }
            pump_messages();
            thread::yield_now();
        }
        // SAFETY: the slot no longer points to the stream and no callback is
        // inside it, so this is the only reference; it came from Box::into_raw.
        let stream = unsafe { Box::from_raw(backend) };
        self.base = self.base.plus(stream.telemetry.counters());
        let _ = card.driver().dispose_all_buffers();
        let Backend { carry, .. } = *stream;
        drop(card);
        Some(carry.into_inner())
    }

    /// Ends the stream for good (a stop, the session end, a structured
    /// exception).
    fn release_for_good(&mut self) -> StopOutcome {
        if let Some(outcome) = self.done {
            return outcome;
        }
        if let Some(live) = self.live.take() {
            self.carry = self.finish(live);
        }
        let outcome = self.done.unwrap_or(StopOutcome::Released);
        self.done = Some(outcome);
        self.publish();
        outcome
    }

    fn fault(&mut self, why: String) {
        if let Some(live) = &self.live {
            // SAFETY: the stream is live.
            unsafe { &*live.backend }
                .faulted
                .store(true, Ordering::Release);
        }
        self.shared.faulted.store(true, Ordering::SeqCst);
        note(&self.shared.fault, why);
    }

    /// One look at the live stream: a panic in the callback, then the reopen
    /// question.
    fn watch(&mut self, live: &mut Live, now: Instant) -> Option<Verdict> {
        // SAFETY: the stream is live.
        let b = unsafe { &*live.backend };
        if b.faulted.load(Ordering::Acquire) {
            if !self.shared.faulted.swap(true, Ordering::SeqCst) {
                note(&self.shared.fault, panic_text());
            }
            return None;
        }
        let asked = b.telemetry.take_reopen();
        let rate_changes = b.telemetry.rate_changes();
        let rate_changed = rate_changes != live.rate_changes;
        live.rate_changes = rate_changes;
        let forced = self.shared.reopen.swap(false, Ordering::SeqCst);
        let stalled = live.watchdog.stalled(b.telemetry.callbacks(), now);
        owner::reset_step(
            asked || rate_changed,
            forced,
            stalled,
            &mut self.budget,
            now,
        )
    }

    /// Finish, then open again with the same processor; its first block
    /// after the reopen follows `Process::discontinuity`.
    fn reopen(&mut self, live: Live) {
        let Some(carry) = self.finish(live) else {
            return;
        };
        if self.shared.release_pending.load(Ordering::SeqCst) {
            // The session ended during the finish: no new open (the next
            // tick releases for good).
            self.carry = Some(carry);
            return;
        }
        if let Err(e) = self.open(carry, false)
            && !matches!(e, AsioError::SessionEnd)
        {
            if matches!(e, AsioError::PrefLeave { .. }) {
                note(&self.shared.pref_failure, e.to_string());
            }
            self.fault(format!("the reopen failed: {e}"));
        }
    }

    fn tick(&mut self, now: Instant) -> Next {
        if SEH.load(Ordering::SeqCst) && self.done.is_none() {
            self.fault("a structured exception reached the filter".to_owned());
            self.release_for_good();
        }
        if self.shared.release_pending.load(Ordering::SeqCst) && self.done.is_none() {
            // The session-end handler could not reach the owner.
            self.release_for_good();
        }
        if self.shared.stop.load(Ordering::SeqCst) {
            let outcome = self.release_for_good();
            self.shared.set_outcome(outcome);
            return match outcome {
                StopOutcome::Released => Next::Exit,
                StopOutcome::Parked => Next::Park,
            };
        }
        if self.done.is_none()
            && let Some(mut live) = self.live.take()
        {
            match self.watch(&mut live, now) {
                None => self.live = Some(live),
                Some(Verdict::Reopen) => self.reopen(live),
                Some(Verdict::Fault) => {
                    self.live = Some(live);
                    self.fault(
                        "the driver needed more reopens than the budget allows (program spec §4.4)"
                            .to_owned(),
                    );
                }
            }
        }
        self.publish();
        Next::Continue
    }

    fn publish(&self) {
        let live = self.live.as_ref().map_or_else(Counters::default, |l| {
            // SAFETY: the stream is live.
            unsafe { &*l.backend }.telemetry.counters()
        });
        let c = self.base.plus(live);
        let s = &self.shared;
        s.callbacks.store(c.callbacks, Ordering::Release);
        s.late.store(c.late, Ordering::Release);
        s.missed.store(c.missed, Ordering::Release);
        s.overruns.store(c.overruns, Ordering::Release);
        s.max_ns.store(c.max_ns, Ordering::Release);
        s.resets
            .store(u64::from(self.budget.used()), Ordering::Release);
    }

    /// The owner thread ends with the driver released: the processor and
    /// its buffers go.
    fn close(&mut self) {
        if let Ok(mut r) = self.shared.ranges.lock() {
            r.clear();
        }
        drop(self.carry.take());
    }
}

/// What the owner thread starts with.
struct Start {
    card: CardConfig,
    frames: u32,
    rx: Vec<u16>,
    tx: Vec<u16>,
    processor: Box<dyn Process>,
    shared: Arc<Shared>,
}

fn owner_main(start: Start, ready: SyncSender<Result<(), AsioError>>) {
    let Start {
        card,
        frames,
        rx,
        tx,
        processor,
        shared,
    } = start;
    let state = Rc::new(RefCell::new(Owner {
        card,
        frames,
        rx,
        tx,
        shared: Arc::clone(&shared),
        budget: ResetBudget::default(),
        base: Counters::default(),
        live: None,
        carry: None,
        done: None,
    }));
    // A hidden top-level window (never message-only: those never see the
    // session end) on this thread, which pumps its messages.
    let window = {
        let state = Rc::clone(&state);
        let at_end = Arc::clone(&shared);
        SessionEndWindow::create(
            SESSION_END_REASON,
            Arc::clone(&shared.session_end),
            move || session_end(&state, &at_end),
        )
    };
    let window = match window {
        Ok(w) => w,
        Err(e) => {
            shared.set_outcome(StopOutcome::Released);
            BUSY.store(false, Ordering::SeqCst);
            let _ = ready.send(Err(AsioError::Call(
                "the session-end window",
                e.to_string(),
            )));
            return;
        }
    };
    let carry = Carry {
        processor,
        inbuf: Vec::new(),
        outbuf: Vec::new(),
    };
    let opened = state.borrow_mut().open(carry, true);
    if let Err(e) = opened {
        if state.borrow().done == Some(StopOutcome::Parked) {
            shared.set_outcome(StopOutcome::Parked);
            let _ = ready.send(Err(e));
            park(&window);
        }
        state.borrow_mut().close();
        shared.set_outcome(StopOutcome::Released);
        BUSY.store(false, Ordering::SeqCst);
        let _ = ready.send(Err(e));
        return;
    }
    state.borrow().publish();
    let _ = ready.send(Ok(()));
    loop {
        pump_messages();
        let next = state.borrow_mut().tick(Instant::now());
        match next {
            Next::Continue => thread::sleep(TICK),
            Next::Exit => break,
            Next::Park => park(&window),
        }
    }
    state.borrow_mut().close();
    drop(window);
    BUSY.store(false, Ordering::SeqCst);
}

/// A parked stream: the thread keeps its window and pumps messages for good
/// (a driver may need them) and never frees the stream.
fn park(_window: &SessionEndWindow) -> ! {
    loop {
        pump_messages();
        thread::sleep(TICK);
    }
}

/// `WM_ENDSESSION` on the owner thread's window: the engine sees
/// `session_ending`, saves, fades out and asks the stream to stop; the
/// driver is released here, before the handler returns and Windows may end
/// the process (after [`owner::SESSION_END_WAIT`] at the latest).
///
/// Dispatched from the pump inside an open or a finish (the owner is
/// borrowed), it cannot reach the driver: it sets `release_pending` and
/// returns, and the owner thread stops that open and releases for good right
/// after. Windows may end the process in between (a short residual window,
/// only when the session ends during an open or a reopen).
fn session_end(state: &RefCell<Owner>, shared: &Shared) {
    let started = Instant::now();
    while !owner::session_end_release(shared.stop.load(Ordering::SeqCst), started.elapsed()) {
        pump_messages();
        thread::sleep(TICK);
    }
    match state.try_borrow_mut() {
        Ok(mut o) => {
            o.release_for_good();
        }
        Err(_) => shared.release_pending.store(true, Ordering::SeqCst),
    }
}

/// The S6 backend (S6 design note §3): the card at the configured buffer,
/// driven by an owner thread that makes every driver call, pumps its
/// messages and watches the stream. The engine's inputs are the topology's
/// RX card channels and its outputs the TX ones, then HIL's spare outputs
/// under the test-signal flag (S6); every other card output is zero on every
/// callback (A1).
pub struct AsioStream<P: Process + 'static> {
    thread: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
    processor: PhantomData<fn(P)>,
}

impl<P: Process + 'static> AsioStream<P> {
    /// Opens the card and starts the owner thread; returns once the period
    /// was measured from the first callbacks, or with the open's refusal.
    /// `rx`/`tx` are the engine's card channels (numbered from 1): the
    /// topology's, and after its TX HIL's spare outputs.
    ///
    /// The first open checks the driver module's holders (I3), opens the
    /// preference window (32 while the driver opens, REAPER's original right
    /// after `createBuffers`), admits the driver (96 kHz, preferred buffer =
    /// `frames`, one sample type) and maps the channels.
    pub fn start(
        card: CardConfig,
        rx: Vec<u16>,
        tx: Vec<u16>,
        processor: P,
    ) -> Result<Self, AsioError> {
        let frames = owner::frames(card.frames).ok_or(AsioError::Frames(card.frames))?;
        // One stream per process (the spike host's claim too).
        if BUSY
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(AsioError::Busy);
        }
        let shared = Arc::new(Shared::default());
        let (ready, answer) = mpsc::sync_channel(1);
        let start = Start {
            card,
            frames,
            rx,
            tx,
            processor: Box::new(processor),
            shared: Arc::clone(&shared),
        };
        let spawned = thread::Builder::new()
            .name("iem-asio-owner".into())
            .spawn(move || owner_main(start, ready));
        let thread = match spawned {
            Ok(t) => t,
            Err(e) => {
                BUSY.store(false, Ordering::SeqCst);
                return Err(AsioError::Thread(e.to_string()));
            }
        };
        match answer.recv_timeout(OPEN_BOUND) {
            Ok(Ok(())) => Ok(Self {
                thread: Some(thread),
                shared,
                processor: PhantomData,
            }),
            Ok(Err(e)) => {
                // A parked open keeps its thread; a released one has ended.
                if shared.outcome() == Some(StopOutcome::Released) {
                    let _ = thread.join();
                }
                Err(e)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // A driver call hangs. The owner thread stops and releases
                // whatever it opened once the call returns (and frees the
                // slot); the engine meanwhile reports the refusal.
                shared.stop.store(true, Ordering::SeqCst);
                Err(AsioError::Thread(format!(
                    "the card did not open within {} s",
                    OPEN_BOUND.as_secs()
                )))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = thread.join();
                let ended =
                    AsioError::Thread("the owner thread ended before the stream opened".to_owned());
                // A driver call panicked inside the preference window and
                // its drop could not close it: the refusal says so.
                Err(
                    match shared.pref_leave.lock().ok().and_then(|p| p.clone()) {
                        Some(error) => AsioError::PrefLeave {
                            error,
                            after: Some(Box::new(ended)),
                        },
                        None => ended,
                    },
                )
            }
        }
    }

    /// The stream's statistics: the measured period, counters summed over
    /// reopens (`max_process_ns` is the longest callback: decode, process
    /// and encode), the reopens, a parked stream and a fault.
    pub fn stats(&self) -> StreamStats {
        let s = &self.shared;
        StreamStats {
            frames: s.frames.load(Ordering::Acquire),
            callbacks: s.callbacks.load(Ordering::Acquire),
            late: s.late.load(Ordering::Acquire),
            missed: s.missed.load(Ordering::Acquire),
            overruns: s.overruns.load(Ordering::Acquire),
            resets: s.resets.load(Ordering::Acquire),
            parked: s.parked.load(Ordering::Acquire) || SEH_PARKED.load(Ordering::SeqCst),
            faulted: s.faulted.load(Ordering::Acquire) || SEH.load(Ordering::SeqCst),
            running: s.running.load(Ordering::Acquire),
            max_process_ns: s.max_ns.load(Ordering::Acquire),
            fault: s.fault.lock().ok().and_then(|f| f.clone()),
        }
    }

    /// `WM_ENDSESSION` reached the owner thread: the engine saves, fades out
    /// and stops the stream (the owner thread releases the driver then).
    pub fn session_ending(&self) -> bool {
        self.shared.session_end.load(Ordering::SeqCst)
    }

    /// Asks for one reopen (HIL, a dev-only flag); it goes through the
    /// reopen budget like a driver's request.
    pub fn force_reopen(&self) {
        self.shared.reopen.store(true, Ordering::SeqCst);
    }

    /// `VirtualLock`s the preallocated engine buffers (after the engine
    /// raised its minimum working set, S1c hand-off). A reopen keeps them.
    pub fn lock_buffers(&self) -> io::Result<()> {
        let ranges = self
            .shared
            .ranges
            .lock()
            .map_err(|_| io::Error::other("the buffer ranges are poisoned"))?;
        for &(addr, len) in ranges.iter() {
            if len > 0 {
                iem_win::power::virtual_lock(ptr::with_exposed_provenance::<u8>(addr), len)?;
            }
        }
        Ok(())
    }

    /// A reopen could not close the preference window: the preferred buffer
    /// may not hold REAPER's original (the engine alarms and exits 3).
    pub fn pref_failure(&self) -> Option<String> {
        self.shared.pref_failure.lock().ok().and_then(|f| f.clone())
    }

    /// Stops the stream: the owner thread stops the driver, waits for the
    /// last callback (bounded), disposes and releases (`Released`), or leaves
    /// a stuck stream allocated (`Parked`).
    pub fn stop(mut self) -> StopOutcome {
        self.halt()
    }

    fn halt(&mut self) -> StopOutcome {
        self.shared.stop.store(true, Ordering::SeqCst);
        let started = Instant::now();
        let outcome = loop {
            let ended = self.thread.as_ref().is_none_or(JoinHandle::is_finished);
            if ended && let Some(t) = self.thread.take() {
                let _ = t.join();
            }
            if let Some(o) = owner::stop_step(self.shared.outcome(), ended, started.elapsed()) {
                break o;
            }
            thread::sleep(TICK);
        };
        match outcome {
            StopOutcome::Released => {
                if let Some(t) = self.thread.take() {
                    let _ = t.join();
                }
            }
            StopOutcome::Parked => {
                // The owner thread keeps pumping for the stuck stream.
                self.shared.parked.store(true, Ordering::SeqCst);
                drop(self.thread.take());
            }
        }
        outcome
    }
}

impl<P: Process + 'static> Drop for AsioStream<P> {
    fn drop(&mut self) {
        if self.thread.is_some() {
            let _ = self.halt();
        }
    }
}

/// Installs the process's SEH filter (S6 design note §3), once, at engine
/// start. On an unhandled structured exception it asks the owner thread to
/// release the driver and waits up to [`owner::SEH_WAIT`] for `RELEASED`
/// (set only after the buffers are disposed and the host is dropped). Then
/// it lets the exception end the process — with
/// `iem_win::errmode::quiet_crashes` in force, without a dialog that could
/// keep the card held in session 1. If the driver is still held, the
/// faulting thread sleeps for good (the stream is parked; the guard alarms).
/// Only the owner-approved `seh_ctl` test exercises it (design §10).
pub fn install_seh_filter() {
    // SAFETY: registers a function of the documented filter signature; the
    // engine installs this filter only, so the previous one is not chained.
    unsafe {
        SetUnhandledExceptionFilter(Some(seh_filter));
    }
}

unsafe extern "system" fn seh_filter(_info: *const EXCEPTION_POINTERS) -> i32 {
    SEH.store(true, Ordering::SeqCst);
    // A callback that faulted never continues (the exception ends the
    // process, or the thread parks below), so its count leaves the stream
    // and the owner thread may release it.
    let depth = DEPTH.with(|d| d.replace(0));
    if depth > 0 {
        BACKEND_IN_FLIGHT.fetch_sub(depth, Ordering::SeqCst);
    }
    let started = Instant::now();
    loop {
        match owner::seh_step(RELEASED.load(Ordering::SeqCst), started.elapsed()) {
            SehStep::Wait => thread::sleep(Duration::from_millis(5)),
            SehStep::Continue => return EXCEPTION_CONTINUE_SEARCH,
            SehStep::Park => {
                SEH_PARKED.store(true, Ordering::SeqCst);
                loop {
                    thread::sleep(Duration::from_secs(3_600));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Silent;

    impl Process for Silent {
        fn process(&mut self, block: &mut Block<'_>) {
            block.zero_outputs();
        }
    }

    fn card(driver: &str, frames: i32) -> CardConfig {
        CardConfig {
            driver: driver.to_owned(),
            module: "testcard.dll".to_owned(),
            frames,
            pref: None,
        }
    }

    // The hosted runner has no ASIO driver: the first open fails at the
    // driver list (or at the name, where one exists), after the module check
    // and before any buffer, and releases everything, so the next start is
    // not `Busy`.
    #[test]
    fn a_missing_driver_refuses_the_stream_and_frees_the_slot() {
        for _ in 0..2 {
            let r = AsioStream::start(
                card("No Such Card", 32),
                (101..=132).collect(),
                (71..=93).collect(),
                Silent,
            );
            assert!(
                matches!(r, Err(AsioError::NoDrivers(_) | AsioError::NotFound { .. })),
                "{:?}",
                r.as_ref().err()
            );
            assert!(RELEASED.load(Ordering::SeqCst));
        }
        assert!(!BUSY.load(Ordering::SeqCst));
    }

    #[test]
    fn a_buffer_that_is_no_sample_count_is_refused_first() {
        for frames in [0, -32] {
            let r = AsioStream::start(card("No Such Card", frames), vec![101], vec![71], Silent);
            assert!(
                matches!(r, Err(AsioError::Frames(f)) if f == frames),
                "{:?}",
                r.as_ref().err()
            );
        }
    }

    #[test]
    fn backend_errors_read_as_sentences() {
        assert_eq!(
            AsioError::Frames(0).to_string(),
            "the configured buffer of 0 samples is not a positive size"
        );
        assert_eq!(
            AsioError::Held(vec![(4242, "test.exe".into()), (7, "other.exe".into())]).to_string(),
            "the driver module is loaded by test.exe (pid 4242), other.exe (pid 7) (I3)"
        );
        assert_eq!(
            AsioError::Period(PeriodVerdict::Wrong {
                expected: 32,
                measured: 64
            })
            .to_string(),
            "the driver delivers 64 samples per callback, expected 32"
        );
        assert_eq!(
            AsioError::Period(PeriodVerdict::Undecided).to_string(),
            "the driver's period could not be measured from its first callbacks"
        );
        let unrestored = AsioError::PrefLeave {
            error: PrefError::Write("denied".into()),
            after: Some(Box::new(AsioError::NoDrivers("none".into()))),
        };
        assert_eq!(
            unrestored.to_string(),
            "the preferred buffer was not restored: writing the preferred buffer failed: \
             denied (after the open failed: no ASIO drivers registered: none)"
        );
        assert_eq!(
            AsioError::Channels(MapError::Zero { side: "rx" }).to_string(),
            "the topology does not fit the card: rx channel 0: card channels count from 1"
        );
        assert_eq!(
            AsioError::SessionEnd.to_string(),
            "the Windows session ended while the card opened"
        );
    }
}
