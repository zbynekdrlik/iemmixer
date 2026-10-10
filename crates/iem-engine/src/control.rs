//! The control loop (program spec §2.3–2.4, I6, I9, I10, X2; design note
//! §3.3, §3.6, §3.7): the one thread that owns the control core. It answers
//! requests, broadcasts revisioned changes, forwards meters and status,
//! schedules saves, clears solos after the controller left, ends test
//! signals, and runs the shutdown and fault paths. It never touches audio
//! except through the command ring.
//!
//! The parts: `connections` (hello, roles, replies, broadcasts, alarms),
//! `requests` (a frame's checks, the core's answer, the RT commands),
//! `status` (meter frames and `Status`), `saving` (the state's saves) and
//! `stop` (shutdown, fault, refused card). Here: the [`Driver`], the loop
//! and its tick.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use iem_audio_io::StreamStats;
use iem_audio_io::hist::HistSnapshot;
use iem_audio_io::owner::StopOutcome;
use iem_engine_proto::{Alarm, AlarmCode, EngineMsg, Role};
use rtrb::Producer;
use tracing::{info, warn};

use crate::cmd::{RtCmd, RtOp};
use crate::core::Core;
use crate::persist::{SaveSchedule, Store};
use crate::pipe::Conn;
use crate::rt::{MeterFrame, RtStatus};

mod connections;
mod requests;
mod saving;
mod status;
mod stop;

pub use self::connections::MAX_ALARMS;
use self::status::meters_msg;

/// The control loop's tick.
pub const TICK: Duration = Duration::from_millis(10);
/// Connections at most (one controller and observers).
pub const MAX_CONNS: usize = 8;
/// How long `Shutdown` waits for the fade-out.
pub const FADE_WAIT: Duration = Duration::from_millis(500);

/// The audio backend as the control loop sees it.
pub trait Driver: Send {
    fn stats(&self) -> StreamStats;
    /// Stops the stream: `Released` once the card is free, `Parked` when
    /// it stayed held (a callback stuck in the stream, or the parked-engine
    /// test's hold; #35). A backend without a card releases.
    fn stop(self: Box<Self>) -> StopOutcome;
    /// Every control tick (never the RT thread): the backend's timed work,
    /// e.g. the ASIO backend locks its memory after 5 s of streaming.
    fn tick(&mut self, _now: Instant) {}
    /// The backend's own reason to end the run, if any.
    fn ending(&self) -> Option<Ending> {
        None
    }
    /// Locking the real-time memory failed (logged by the backend).
    fn lock_failed(&self) -> bool {
        false
    }
    /// HIL's forced reopen (`Cmd::ForceReopen`): whether the backend took it
    /// (the ASIO card reopens through its reset budget; NullRt has no card).
    fn force_reopen(&self) -> bool {
        false
    }
    /// The stream's histograms since it opened (S7 design note §3), read once
    /// a second for `Status`; none from a backend without them.
    fn histograms(&self) -> Option<HistSnapshot> {
        None
    }
}

/// Why a backend ends the run (S6 design note §3, §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ending {
    /// The Windows session ends: save, fade out, release (the shutdown path).
    Session,
    /// The card must be refused (exit 3), e.g. a release could not write
    /// REAPER's preferred buffer back.
    Card(String),
}

/// Messages from the acceptor and reader threads.
#[derive(Debug)]
pub enum CtlMsg {
    Connected { id: u64, conn: Conn },
    Frame { id: u64, bytes: Vec<u8> },
    Closed { id: u64, why: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exit {
    /// `faded`: the output reached silence before the driver stopped.
    Shutdown {
        faded: bool,
    },
    Fault(String),
    /// The backend refused the card while running (exit 3).
    Card(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// X2: solos clear this long after the controller left.
    pub solo_grace: Duration,
    pub block: u32,
    /// Started with `--hold`: silent until the supervisor's `Arm`.
    pub hold: bool,
}

struct Peer {
    conn: Conn,
    role: Option<Role>,
}

pub struct Control {
    core: Core,
    store: Store,
    schedule: SaveSchedule,
    cmds: Producer<RtCmd>,
    pending: VecDeque<Vec<RtOp>>,
    meters: triple_buffer::Output<MeterFrame>,
    status: Arc<RtStatus>,
    talkback_dropped: Arc<AtomicU64>,
    driver: Option<Box<dyn Driver>>,
    peers: BTreeMap<u64, Peer>,
    controller: Option<u64>,
    controller_lost: Option<Instant>,
    /// The guard's connection (S6): one at a time, beside the controller.
    supervisor: Option<u64>,
    /// `--hold` until the supervisor's `Arm`.
    held: bool,
    test_deadline: Option<Instant>,
    counters: Vec<u64>,
    alarms: Vec<Alarm>,
    last_status: Instant,
    settings: Settings,
    shutdown: bool,
    /// Sanitiser trips already alarmed.
    trips_seen: u64,
    /// HIL's spare outputs (S6, `Core::hil` order): the largest peak of the
    /// meter frames since the last `Status`.
    hil_peaks: Vec<f64>,
}

/// Everything `Control` needs from the engine's start-up.
pub struct Parts {
    pub core: Core,
    pub store: Store,
    pub cmds: Producer<RtCmd>,
    pub meters: triple_buffer::Output<MeterFrame>,
    pub status: Arc<RtStatus>,
    pub talkback_dropped: Arc<AtomicU64>,
    pub driver: Box<dyn Driver>,
    pub counters: Vec<u64>,
    pub alarms: Vec<Alarm>,
    pub settings: Settings,
}

impl Control {
    pub fn new(p: Parts) -> Self {
        let hil_peaks = vec![0.0; p.core.hil().len()];
        Self {
            core: p.core,
            store: p.store,
            schedule: SaveSchedule::default(),
            cmds: p.cmds,
            pending: VecDeque::new(),
            meters: p.meters,
            status: p.status,
            talkback_dropped: p.talkback_dropped,
            driver: Some(p.driver),
            peers: BTreeMap::new(),
            controller: None,
            controller_lost: None,
            supervisor: None,
            held: p.settings.hold,
            test_deadline: None,
            counters: p.counters,
            alarms: p.alarms,
            last_status: Instant::now(),
            settings: p.settings,
            shutdown: false,
            trips_seen: 0,
            hil_peaks,
        }
    }

    /// Runs until `Shutdown` or a fault.
    pub fn run(mut self, rx: &Receiver<CtlMsg>) -> Exit {
        loop {
            match rx.recv_timeout(TICK) {
                Ok(msg) => self.handle(msg),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => std::thread::sleep(TICK),
            }
            while !self.shutdown
                && let Ok(msg) = rx.try_recv()
            {
                self.handle(msg);
            }
            if let Some(exit) = self.tick(Instant::now()) {
                return exit;
            }
        }
    }

    fn handle(&mut self, msg: CtlMsg) {
        match msg {
            CtlMsg::Connected { id, conn } => {
                if self.peers.len() >= MAX_CONNS {
                    warn!("connection {id} refused: {MAX_CONNS} connections already");
                    conn.close();
                    return;
                }
                info!("connection {id} opened");
                self.peers.insert(id, Peer { conn, role: None });
            }
            CtlMsg::Closed { id, why } => self.drop_peer(id, &why),
            CtlMsg::Frame { id, bytes } => self.frame(id, &bytes),
        }
    }

    fn tick(&mut self, now: Instant) -> Option<Exit> {
        self.flush_rt();
        if let Some(d) = self.driver.as_mut() {
            d.tick(now);
        }
        let stats = self.driver.as_ref().map(|d| d.stats()).unwrap_or_default();
        if stats.faulted {
            // The last `Status` first (S7 HIL v2): it carries the faulting
            // callback's time, which the guard keeps across the respawn.
            let last = self.next_status(&stats);
            self.broadcast(&EngineMsg::Status(last));
            return Some(self.fault(stats.fault.unwrap_or_else(|| "unknown".into())));
        }
        match self.driver.as_ref().and_then(|d| d.ending()) {
            Some(Ending::Card(why)) => return Some(self.card(why)),
            Some(Ending::Session) => {
                info!("the Windows session ends");
                self.shutdown = true;
            }
            None => {}
        }
        if self.shutdown {
            return Some(self.shutdown_now());
        }
        if self.meters.updated() {
            let frame = self.meters.read().clone();
            if frame.trips > self.trips_seen {
                self.trips_seen = frame.trips;
                self.alarm(
                    AlarmCode::Sanitizer,
                    format!("sanitiser trips: {}", frame.trips),
                );
            }
            self.counters.clone_from(&frame.active);
            self.note_hil(&frame.hil);
            self.broadcast(&EngineMsg::Meters(meters_msg(&frame)));
        }
        if now.saturating_duration_since(self.last_status) >= Duration::from_secs(1) {
            self.last_status = now;
            let status = self.next_status(&stats);
            self.broadcast(&EngineMsg::Status(status));
        }
        if self.controller.is_none()
            && let Some(lost) = self.controller_lost
            && now.saturating_duration_since(lost) >= self.settings.solo_grace
        {
            self.controller_lost = None;
            let out = self.core.clear_solos();
            if !out.changes.is_empty() {
                info!(
                    "solos cleared: no controller for {:?}",
                    self.settings.solo_grace
                );
            }
            self.publish(out, None);
        }
        if let Some(deadline) = self.test_deadline
            && now >= deadline
        {
            self.test_deadline = None;
            let out = self.core.end_test_signal();
            self.publish(out, None);
        }
        if self.schedule.due(now) {
            self.save();
        }
        None
    }
}

#[cfg(test)]
mod tests;
