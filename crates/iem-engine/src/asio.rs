//! The ASIO backend's start-up on Windows (S6 design note §3, §4): the card
//! of the site's `[card]` table behind [`Driver`].
//!
//! Windows effects only, excluded from mutation (`.cargo/mutants.toml`);
//! the decisions are portable and tested: the memory lock's timing
//! ([`lock_due`]), the backend's endings ([`Ending`], `Control::tick`), the
//! card's channels, period and preference window (`iem_audio_io`,
//! `iem_win::prefwin`).

use std::io;
use std::time::Instant;

use iem_audio_io::StreamStats;
use iem_audio_io::asio::{AsioError, AsioStream, CardConfig, StopOutcome, install_seh_filter};
use iem_audio_io::hist::HistSnapshot;
use tracing::{error, info, warn};

use crate::control::{Driver, Ending};
use crate::engine::{EngineError, LOCK_EXTRA_MB, lock_due};
use crate::rt::Processor;
use crate::site::Card;
use crate::topology::{Topology, hil_return_refusal};

/// Before the card opens (S6 design note §3): no crash dialog that could
/// keep the driver held in session 1, the RT panic hook (atomics only), the
/// SEH filter that releases the driver first, HIGH priority, power
/// throttling off and the `[card]` CPU Sets (S1c L5). A failure is logged,
/// never fatal.
pub(crate) fn prepare(card: &Card) {
    if let Err(e) = iem_win::errmode::quiet_crashes() {
        warn!("the crash dialogs could not be turned off: {e}");
    }
    iem_audio_io::rtpanic::install();
    install_seh_filter();
    if let Err(e) = iem_win::power::set_high_priority() {
        warn!("HIGH priority failed: {e}");
    }
    if let Err(e) = iem_win::power::disable_power_throttling() {
        warn!("turning power throttling off failed: {e}");
    }
    if !card.cpu_sets.is_empty() {
        match iem_win::power::set_cpu_sets(&card.cpu_sets) {
            Ok(()) => info!("CPU Sets {:?}", card.cpu_sets),
            Err(e) => warn!("CPU Sets {:?} failed: {e}", card.cpu_sets),
        }
    }
}

fn card_config(card: &Card) -> CardConfig {
    CardConfig {
        driver: card.driver.clone(),
        module: card.module.clone(),
        frames: i32::try_from(card.frames).unwrap_or(i32::MAX),
        pref: Some((
            card.pref_key.clone(),
            card.pref_name.clone(),
            card.pref_original(),
        )),
    }
}

/// A refused card is exit 3 (the guard never respawns): a holder of the
/// driver module (I3), no such driver, the format, the channel map, the
/// preference window, the measured period. A failing driver call or thread
/// is an i/o error (exit 1).
fn refusal(e: AsioError) -> EngineError {
    if let AsioError::Channels(m) = &e
        && let Some(why) = hil_return_refusal(m)
    {
        return EngineError::Card(why);
    }
    match e {
        AsioError::NoDrivers(_)
        | AsioError::NotFound { .. }
        | AsioError::Refused(_)
        | AsioError::Frames(_)
        | AsioError::Held(_)
        | AsioError::Scan(_)
        | AsioError::Channels(_)
        | AsioError::Pref(_)
        | AsioError::PrefLeave { .. }
        | AsioError::Period(_) => EngineError::Card(e.to_string()),
        AsioError::Create(_)
        | AsioError::Init(_)
        | AsioError::Call(..)
        | AsioError::Busy
        | AsioError::Thread(_)
        | AsioError::SessionEnd => EngineError::Io(io::Error::other(e.to_string())),
    }
}

fn log_stop(outcome: StopOutcome) {
    match outcome {
        StopOutcome::Released => info!("the card is released"),
        StopOutcome::Parked => {
            error!("a callback is stuck: the stream stays parked and may hold the card");
        }
    }
}

/// The running card as the control loop sees it.
struct AsioDriver {
    stream: AsioStream<Processor>,
    started: Instant,
    /// The memory lock was tried (once).
    lock_tried: bool,
    lock_failed: bool,
}

impl Driver for AsioDriver {
    fn stats(&self) -> StreamStats {
        self.stream.stats()
    }

    fn stop(self: Box<Self>) -> StopOutcome {
        let AsioDriver { stream, .. } = *self;
        let outcome = stream.stop();
        log_stop(outcome);
        outcome
    }

    /// After 5 s of streaming: a larger, hard minimum working set, then the
    /// preallocated RT buffers locked (S1c hand-off). A failure is logged and
    /// reported in Status, never fatal.
    fn tick(&mut self, now: Instant) {
        if !lock_due(self.started, now, self.lock_tried) {
            return;
        }
        self.lock_tried = true;
        let locked = iem_win::power::lock_min_working_set(LOCK_EXTRA_MB)
            .and_then(|()| self.stream.lock_buffers());
        match locked {
            Ok(()) => info!("RT memory locked (working set +{LOCK_EXTRA_MB} MiB)"),
            Err(e) => {
                self.lock_failed = true;
                warn!("locking the RT memory failed: {e}");
            }
        }
    }

    fn ending(&self) -> Option<Ending> {
        if let Some(why) = self.stream.pref_failure() {
            return Some(Ending::Card(why));
        }
        self.stream.session_ending().then_some(Ending::Session)
    }

    fn lock_failed(&self) -> bool {
        self.lock_failed
    }

    /// HIL's forced reopen: the owner thread reopens through the reset
    /// budget (`Status.resets` counts it; the fade-in restarts).
    fn force_reopen(&self) -> bool {
        self.stream.force_reopen();
        true
    }

    /// Counted since `AsioStream::start`, the card's reopens included (S7).
    fn histograms(&self) -> Option<HistSnapshot> {
        Some(self.stream.histograms())
    }
}

/// Opens the card for `run --backend asio` with the topology's channels
/// and, after its TX, HIL's spare outputs `hil` (S6, empty without the
/// test-signal flag; a channel the card lacks refuses the stream): the
/// driver and the block size (the card's frames).
pub(crate) fn start(
    card: &Card,
    topo: &Topology,
    hil: &[u16],
    processor: Processor,
) -> Result<(Box<dyn Driver>, usize), EngineError> {
    let outputs: Vec<u16> = topo.tx.iter().chain(hil).copied().collect();
    let stream = AsioStream::start(
        card_config(card),
        topo.rx.clone(),
        outputs,
        hil.to_vec(),
        processor,
    )
    .map_err(refusal)?;
    info!(
        "ASIO running: {} at {} samples (measured {}), {} RX, {} TX, {} HIL",
        card.driver,
        card.frames,
        stream.stats().frames,
        topo.rx.len(),
        topo.tx.len(),
        hil.len()
    );
    let driver: Box<dyn Driver> = Box::new(AsioDriver {
        stream,
        started: Instant::now(),
        lock_tried: false,
        lock_failed: false,
    });
    let block = usize::try_from(card.frames).unwrap_or(usize::MAX);
    Ok((driver, block))
}
