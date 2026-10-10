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
//!   so do its driver-message handlers, which may run on any thread: they
//!   count, and the owner thread logs. Its decisions live in the portable,
//!   mutation-tested `format`, `telemetry`, `channels`, `period`, `reset`,
//!   `owner`, `messages` and `iem_win::prefwin`.
//!
//! The module is split by responsibility (#36). This file holds the error,
//! the driver layer ([`Host`]: open, info, latencies), the buffer helpers
//! both streams share, and the S6 backend's state that its parts share (the
//! process-wide statics, [`CardConfig`], the stream, card, owner and start
//! structs), so each part reads their private fields as a child module. The
//! parts: `spike` (the S1a spike's stream, [`Running`], and its callbacks),
//! `backend` (the S6 stream's real-time callbacks), `owner_thread` (the
//! owner thread: opens, the watch, reopen, release), `pref` (the preference
//! window), `stream` ([`AsioStream`], the engine's handle and its stats) and
//! `seh` (the SEH filter and the test exceptions).

mod backend;
mod owner_thread;
mod pref;
mod seh;
mod spike;
mod stream;

pub use seh::{install_seh_filter, raise_test_park, raise_test_seh};
pub use spike::{Running, STOP_WAIT, StartTimings, StopTimings, StreamConfig};
pub use stream::AsioStream;

use core::cell::{Cell, UnsafeCell};
use core::ffi::{CStr, c_void};
use core::fmt;
use core::ptr;
use core::sync::atomic::{
    AtomicBool, AtomicI64, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering,
};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use azo::dto::{ChannelId, Granularity};
use azo::sys::Bool;
use azo::utils::com::InitGuard;
// Re-exported: `CardConfig::pref` and the preference errors name them.
pub use iem_win::prefwin::{self, Pref, PrefError};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
};

use crate::Process;
use crate::channels::MapError;
use crate::format::{self, Refusal, SampleFormat};
use crate::hist::StreamHists;
use crate::messages::{self, Messages, TOPICS};
pub use crate::owner::StopOutcome;
use crate::owner::{self, Watchdog};
use crate::period::PeriodVerdict;
use crate::reset::ResetBudget;
use crate::telemetry::{Counters, Telemetry};
use pref::PrefWindow;

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

    /// Reads the latencies the driver reports now (samples).
    pub fn latencies(&self) -> Result<(i32, i32), AsioError> {
        let l = self.driver.latencies().map_err(self.call("getLatencies"))?;
        Ok((l.in_, l.out))
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

/// A stream is claimed (from `start` until `finish` freed it).
static BUSY: AtomicBool = AtomicBool::new(false);

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
/// No driver instance of the backend exists: cleared just before an open
/// creates one, set once it is dropped (after its buffers were disposed).
/// The SEH filter waits for it; a parked stream never sets it.
static RELEASED: AtomicBool = AtomicBool::new(true);
/// A structured exception reached the filter: the owner thread releases the
/// driver (under [`SEH_HOLD`] it keeps it).
static SEH: AtomicBool = AtomicBool::new(false);
/// The filter parked a faulting thread (the driver was not released in time).
static SEH_PARKED: AtomicBool = AtomicBool::new(false);
/// The parked-engine test's hold (S6 design §10 test #2, #35): set by
/// [`raise_test_park`] before its exception, never cleared. The owner thread
/// then keeps the driver after a structured exception (`owner::seh_release`)
/// and the exception is no fault (`owner::seh_faults`). Dev-only: reached
/// only through the engine's fault-injection flag and the guard's HIL job.
static SEH_HOLD: AtomicBool = AtomicBool::new(false);
/// Every driver message since the process started: counted by the handlers
/// on whatever thread the driver calls them, logged by the owner thread
/// (`crate::messages`; #9 2026-09-28).
static MESSAGES: Messages = Messages::new();
/// The owner thread's clock, set before its first driver exists: the
/// messages' times and the owner's log lines read it.
static BASE: OnceLock<Instant> = OnceLock::new();

/// Now on the owner thread's clock (0 before it started): an atomic load
/// and the performance counter, no allocation or lock (the handlers).
fn clock_ns() -> u64 {
    BASE.get().map_or(0, |base| nanos(base.elapsed()))
}

/// Now on the owner thread's clock, for a log line.
fn when() -> String {
    messages::stamp(clock_ns())
}

thread_local! {
    /// How deep this thread is inside the backend's callbacks: the SEH filter
    /// takes a faulting callback's count out of `BACKEND_IN_FLIGHT`.
    static DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// What a reopen carries from one stream to the next: the processor and its
/// preallocated buffers (the same pages, so a `VirtualLock` holds).
struct Carry {
    processor: Box<dyn Process>,
    /// Engine inputs, channel-major: `Topology::rx` order, then the D5(b)
    /// loopback return (S6 test 5).
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
    /// The stream histograms (S7): the owner's, cloned in at the open, so
    /// they count across reopens. The callback only increments them (I7).
    hists: Arc<StreamHists>,
    base: Instant,
    /// The sample positions of the first callbacks (the open's period).
    ring: [AtomicI64; owner::RING],
    ring_len: AtomicUsize,
    /// Set at a reopen: the callback calls `Process::discontinuity` first.
    discontinuity: AtomicBool,
    /// A panic in the processor (or the owner's fault): the outputs stay
    /// zero and the processor is never called again.
    faulted: AtomicBool,
    /// The faulting callback's own time, ns (S7 HIL v2): stored by that
    /// callback right before `faulted`, so the owner reads it with the flag;
    /// 0 for the owner's own fault.
    fault_ns: AtomicU64,
    output_ready: AtomicBool,
    driver: *const azo::Driver,
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
/// before the mark sets `RELEASED` (`Owner::release_card` closes the
/// preference window in between).
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
    /// The last reopen's time, µs (S7 HIL v2; `owner::reopen_us`).
    reopen_us: AtomicU64,
    /// The faulting callback's own time, ns (S7 HIL v2), copied from the
    /// stream before `faulted` is set here.
    fault_ns: AtomicU64,
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

/// The owner thread's state: the only code that calls the driver.
struct Owner {
    card: CardConfig,
    frames: u32,
    rx: Vec<u16>,
    tx: Vec<u16>,
    /// The D5(b) loopback return card inputs (S6 test 5), opened after `rx`.
    hil_rx: Vec<u16>,
    shared: Arc<Shared>,
    /// The stream histograms (S7), handed to every open's backend.
    hists: Arc<StreamHists>,
    budget: ResetBudget,
    /// The counters of the streams already closed.
    base: Counters,
    live: Option<Live>,
    /// The processor and its buffers while no stream is open.
    carry: Option<Carry>,
    /// Finished for good: by a stop, the session end or a structured
    /// exception, or parked.
    done: Option<StopOutcome>,
    /// The opens so far, the first included (the log numbers them).
    opens: u32,
    /// The driver messages already logged, per topic.
    seen: [u64; TOPICS],
    /// The preference window (`None` only in tests). Last: when the owner
    /// thread unwinds, the live card is dropped first, then the window's
    /// `Drop` writes REAPER's value back.
    pref: Option<PrefWindow>,
}

/// What the owner thread starts with.
struct Start {
    card: CardConfig,
    frames: u32,
    rx: Vec<u16>,
    tx: Vec<u16>,
    hil_rx: Vec<u16>,
    processor: Box<dyn Process>,
    shared: Arc<Shared>,
    hists: Arc<StreamHists>,
}

#[cfg(test)]
mod tests;
