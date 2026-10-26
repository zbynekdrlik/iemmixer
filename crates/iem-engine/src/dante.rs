//! The engine's Dante backend on Linux (Gen 3 phase C, #72; plan
//! `docs/superpowers/plans/2026-10-26-gen3-phase-c-engine-on-linux.md`
//! Task 3; design §5.1, §5.2): `run --backend dante` hosts our own Dante
//! device (`iem_dante::host`) in the engine's process and drives the
//! processor on its media clock (`iem_dante::stream`'s pump), as the ASIO
//! backend (`asio.rs`, Windows) drives it on the card's callback.
//!
//! - [`check`] (pure): the device must run at the engine's 96 kHz, and the
//!   site's channel numbers are the device's own (site RX n = our RX
//!   channel n, site TX n = our TX channel n; HIL's spare outputs after the
//!   topology's TX and, under `--test-signal`, their loopback returns after
//!   its RX, through the portable [`ChannelMap`] the ASIO path uses): a
//!   channel the device lacks refuses the start (exit 3, as a card that
//!   lacks one).
//! - [`start`] (glue): the device set up with an engine ring on each mapped
//!   TX channel, those channels' sources set to the engine, the pump
//!   attached to the device's control socket and spawned on the media
//!   clock with D = B + P + W ([`timing`]), the device served with SIGTERM
//!   and SIGINT reported to the engine, never acted on by the device.
//! - [`DanteDriver`]: the control loop's [`Driver`]: the pump's counts and
//!   histograms, the endings ([`Ending::Stop`] once a signal was reported,
//!   [`Ending::Backend`] once the device ended by itself), and the stop:
//!   the pump first (within [`PUMP_WITHIN`]), then the device (every RX
//!   flow stopped at its transmitter, the goodbyes; within
//!   [`STOP_WITHIN`]), each left to end with a warning when late, never
//!   killed (I8).
//!
//! Logs name counts, channel numbers and the topology hash, never a name or
//! an address. Every decision here is pure and tested (`dante/tests.rs`);
//! [`start`] and the [`Host`] behind [`Device`] are excluded from mutation
//! (`.cargo/mutants.toml`): the real device needs an interface with a MAC
//! (the box), which the unit tests never have.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use iem_audio_io::StreamStats;
use iem_audio_io::channels::{ChannelMap, MapError};
use iem_audio_io::hist::HistSnapshot;
use iem_audio_io::owner::StopOutcome;
use iem_dante::clock::to_samples;
use iem_dante::daemon::{Options, Place};
use iem_dante::device::DeviceConfig;
use iem_dante::host::{self, Host, HostError, Signals, Stopped};
use iem_dante::media::source::Source;
use iem_dante::stream::{self, BLOCK, Map, Pump, PumpStats, Timing, WORK_MARGIN};
use tracing::{info, warn};

use crate::SAMPLE_RATE;
use crate::control::{Driver, Ending};
use crate::engine::{DanteArgs, DantePlace, EngineError};
use crate::rt::Processor;
use crate::topology::{Topology, hil_return_refusal};

/// How long the stop waits for the pump's thread: it ends within one block
/// (or one free period) of its stop flag.
pub const PUMP_WITHIN: Duration = Duration::from_secs(1);
/// How long the stop waits for the device: its RX flows' stops (1 s), its
/// media threads (1 s) and the goodbyes (3 s), inside the unit's 10 s.
pub const STOP_WITHIN: Duration = Duration::from_secs(7);

/// The device's channels for the engine's inputs and outputs (pure): the
/// device must run at the engine's rate ([`EngineError::Card`] otherwise),
/// the site's RX channels and then, under `--test-signal`, HIL's loopback
/// returns `hil` are the engine's inputs, the site's TX channels and then
/// HIL's spare outputs `hil` its outputs, each the device channel of the
/// same number (indices from 0). A channel the device lacks is
/// [`EngineError::Card`] (exit 3).
pub fn check(cfg: &DeviceConfig, topo: &Topology, hil: &[u16]) -> Result<Map, EngineError> {
    if cfg.sample_rate != SAMPLE_RATE {
        return Err(EngineError::Card(format!(
            "the Dante device runs at {} Hz; the engine runs at {SAMPLE_RATE} Hz only (I2)",
            cfg.sample_rate
        )));
    }
    let refused = |e: MapError| {
        EngineError::Card(
            hil_return_refusal(&e).unwrap_or_else(|| format!("the Dante device: {e}")),
        )
    };
    let outputs: Vec<u16> = topo.tx.iter().chain(hil).copied().collect();
    let map = ChannelMap::new(&topo.rx, &outputs, cfg.rx.len(), cfg.tx.len())
        .and_then(|m| m.with_hil_return(hil, cfg.rx.len()))
        .map_err(refused)?;
    Ok(Map {
        rx: map.all_rx(),
        tx: map.tx().to_vec(),
    })
}

/// The pump's timing on the device `cfg`: B = 32, P = the device's latency
/// in samples (the playout point its RX checks read at), W = 1 ms.
pub fn timing(cfg: &DeviceConfig) -> Timing {
    Timing {
        block: BLOCK,
        playout: to_samples(cfg.latency_ns, cfg.sample_rate),
        margin: WORK_MARGIN,
    }
}

/// The device's options from the engine's `--dante-*` flags.
pub fn options(args: &DanteArgs) -> Options {
    Options {
        config: args.config.clone(),
        place: match &args.place {
            DantePlace::Iface(name) => Place::Iface(name.clone()),
            DantePlace::Ip(ip) => Place::Ip(*ip),
        },
        control: args.control.clone(),
    }
}

/// The device's errors as the engine's exits: its setup (the config, the
/// state file, an interface without a usable MAC) is a usage error (exit
/// 2, not restarted), its running (an address not there yet, a socket,
/// serving) an i/o error (exit 1, restarted), as `iem-dante-dev` exits.
pub fn host_error(e: HostError) -> EngineError {
    match e {
        HostError::Setup(why) => EngineError::Usage(format!("the Dante device: {why}")),
        HostError::Run(why) => {
            EngineError::Io(io::Error::other(format!("the Dante device: {why}")))
        }
    }
}

/// The running device as the driver stops it: the real [`Host`], a fake in
/// the tests.
pub trait Device: Send {
    /// Why serving ended by itself, once it did ([`Host::ended`]).
    fn ended(&self) -> Option<String>;
    /// Ends serving and waits up to `within` ([`Host::stop`]).
    fn stop(self: Box<Self>, within: Duration) -> Stopped;
}

impl Device for Host {
    fn ended(&self) -> Option<String> {
        Host::ended(self)
    }

    fn stop(self: Box<Self>, within: Duration) -> Stopped {
        Host::stop(*self, within)
    }
}

/// Waits up to `within` for `thread` to end, a 1 ms nap a pass, and joins
/// it: whether it ended (a panic in it is logged and counts as ended). One
/// still running is left to end, never killed.
pub fn join_within(thread: JoinHandle<()>, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while !thread.is_finished() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    if thread.join().is_err() {
        warn!("the engine pump's thread panicked");
    }
    true
}

/// The Dante backend as the control loop sees it (see the module).
pub struct DanteDriver {
    stats: Arc<PumpStats>,
    pump: JoinHandle<()>,
    /// Ends the pump's thread.
    pump_stop: Arc<AtomicBool>,
    /// Set by the device's runtime at SIGTERM or SIGINT.
    reported: Arc<AtomicBool>,
    device: Box<dyn Device>,
}

impl DanteDriver {
    /// The driver of a pump counting into `stats` on its thread `pump`
    /// (ended by `pump_stop`) and of the `device` serving beside it, which
    /// sets `reported` at a signal.
    pub fn new(
        stats: Arc<PumpStats>,
        pump: JoinHandle<()>,
        pump_stop: Arc<AtomicBool>,
        reported: Arc<AtomicBool>,
        device: Box<dyn Device>,
    ) -> Self {
        DanteDriver {
            stats,
            pump,
            pump_stop,
            reported,
            device,
        }
    }
}

impl Driver for DanteDriver {
    /// The pump as a stream: B = 32, its blocks, late and skipped blocks,
    /// overruns and re-bootstraps, its fault.
    fn stats(&self) -> StreamStats {
        self.stats.stream_stats()
    }

    /// The pump stops first (its flag; joined within [`PUMP_WITHIN`], a
    /// warning when it is not), so nothing writes the engine rings while
    /// the device ends; then the device ([`Device::stop`] within
    /// [`STOP_WITHIN`]: its RX flows stopped at their transmitters, its
    /// media threads, the goodbyes). The device holds no card: released.
    fn stop(self: Box<Self>) -> StopOutcome {
        let DanteDriver {
            pump,
            pump_stop,
            device,
            ..
        } = *self;
        pump_stop.store(true, Ordering::Release);
        if !join_within(pump, PUMP_WITHIN) {
            warn!(
                wait_ms = PUMP_WITHIN.as_millis(),
                "the engine pump did not end within the wait: left to end"
            );
        }
        match device.stop(STOP_WITHIN) {
            Stopped::Done => info!("the Dante device stopped (RX flows stopped, goodbyes sent)"),
            Stopped::Failed(why) => warn!("the Dante device ended badly: {why}"),
            Stopped::StillRunning => warn!(
                wait_ms = STOP_WITHIN.as_millis(),
                "the Dante device did not end within the wait: left to end"
            ),
        }
        StopOutcome::Released
    }

    /// The device that ended by itself first (the run ends with exit 1),
    /// then a reported SIGTERM or SIGINT (the shutdown path).
    fn ending(&self) -> Option<Ending> {
        if let Some(why) = self.device.ended() {
            return Some(Ending::Backend(why));
        }
        self.reported
            .load(Ordering::Acquire)
            .then_some(Ending::Stop)
    }

    /// The pump's block intervals and block times since it started.
    fn histograms(&self) -> Option<HistSnapshot> {
        Some(self.stats.histograms())
    }
}

/// Starts the Dante backend for `run --backend dante` (glue: every decision
/// is [`check`]'s, [`timing`]'s, [`host_error`]'s, the [`DanteDriver`]'s and
/// the pump's): the device's config read and mapped, the device set up
/// with an engine ring on each mapped TX channel and those channels' sources
/// set to the engine, the pump attached and spawned on the media clock with
/// `processor` (outputs quantized at the device's encoding), the device
/// served with SIGTERM and SIGINT reported. The driver and the block (B =
/// 32). Called outside any tokio runtime (the device's own runtime runs on
/// this thread while it is set up).
pub fn start(
    args: &DanteArgs,
    topo: &Topology,
    hil: &[u16],
    processor: Processor,
) -> Result<(Box<dyn Driver>, usize), EngineError> {
    let opts = options(args);
    let map = check(
        &host::read_config(&opts.config).map_err(host_error)?,
        topo,
        hil,
    )?;
    let prepared = host::prepare(&opts, &map.tx).map_err(host_error)?;
    let cfg = prepared.config().clone();
    if check(&cfg, topo, hil)? != map {
        return Err(EngineError::Usage(
            "the Dante device's config changed while the engine started".to_owned(),
        ));
    }
    let timing = timing(&cfg);
    let media = prepared.media();
    for &tx in &map.tx {
        if !media.tx.set_source(tx, Source::Engine) {
            return Err(EngineError::Io(io::Error::other(format!(
                "the Dante device's TX channel {} has no engine ring",
                tx + 1
            ))));
        }
    }
    let stats = Arc::new(PumpStats::new(timing, cfg.sample_rate));
    media.attach_engine(Arc::clone(&stats));
    let (inputs, outputs) = (map.rx.len(), map.tx.len());
    let pump = Pump::new(
        processor,
        map,
        timing,
        cfg.encoding,
        Arc::clone(&media.rx),
        Arc::clone(&media.tx),
        Arc::clone(&stats),
    );
    let pump_stop = Arc::new(AtomicBool::new(false));
    let thread = stream::spawn(Arc::clone(&media.clock), pump, Arc::clone(&pump_stop))?;
    let reported = Arc::new(AtomicBool::new(false));
    let device = match prepared.start(Signals::Report(Arc::clone(&reported))) {
        Ok(device) => device,
        Err(e) => {
            pump_stop.store(true, Ordering::Release);
            if !join_within(thread, PUMP_WITHIN) {
                warn!("the engine pump did not end within the wait: left to end");
            }
            return Err(host_error(e));
        }
    };
    info!(
        topology = %topo.hash,
        inputs,
        outputs,
        offset = timing.offset(),
        playout = timing.playout,
        "Dante backend running: the engine on the device's media clock, 32 samples a block, \
         outputs at RX time + offset"
    );
    let driver = DanteDriver::new(stats, thread, pump_stop, reported, Box::new(device));
    Ok((Box::new(driver), BLOCK as usize))
}

#[cfg(test)]
mod tests;
