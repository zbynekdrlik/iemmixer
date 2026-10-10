//! The control loop (program spec §2.3–2.4, I6, I9, I10, X2; design note
//! §3.3, §3.6, §3.7): the one thread that owns the control core. It answers
//! requests, broadcasts revisioned changes, forwards meters and status,
//! schedules saves, clears solos after the controller left, ends test
//! signals, and runs the shutdown and fault paths. It never touches audio
//! except through the command ring.

use std::collections::{BTreeMap, VecDeque};
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use iem_audio_io::StreamStats;
use iem_audio_io::hist::HistSnapshot;
use iem_audio_io::owner::StopOutcome;
use iem_engine_proto::{
    Alarm, AlarmCode, ClientMsg, Cmd, EngineMsg, ErrCode, ErrorBody, Hello, HilOut, Meters, PROTO,
    Reply, Role, Status, negotiate, parse_client, write_frame,
};
use rtrb::Producer;
use tracing::{error, info, warn};

use crate::cmd::{RtCmd, RtOp, push_group};
use crate::core::{Core, Effect, Outcome};
use crate::persist::{Persisted, SaveSchedule, Store};
use crate::pipe::Conn;
use crate::rt::{MeterFrame, RtStatus};
use crate::{MAX_CMDS_PER_BLOCK, SAMPLE_RATE};

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

fn engine_build() -> String {
    format!(
        "{}+{}",
        env!("CARGO_PKG_VERSION"),
        option_env!("GITHUB_SHA").unwrap_or("local")
    )
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn encode(msg: &EngineMsg) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    match write_frame(&mut out, msg) {
        Ok(()) => Some(out),
        Err(e) => {
            error!("cannot encode an engine message: {e}");
            None
        }
    }
}

/// Alarms replayed to each new connection: the newest `MAX_ALARMS`.
pub const MAX_ALARMS: usize = 16;

fn push_alarm(list: &mut Vec<Alarm>, alarm: Alarm) {
    list.push(alarm);
    if list.len() > MAX_ALARMS {
        list.remove(0);
    }
}

fn meters_msg(f: &MeterFrame) -> Meters {
    let pair = |&[l, r]: &[f64; 2]| [l as f32, r as f32];
    Meters {
        seq: f.seq,
        inputs: f.inputs.iter().map(pair).collect(),
        mixes: f.mixes.iter().map(pair).collect(),
        groups: f.groups.iter().map(pair).collect(),
        gr_db: f.gr_db.iter().map(|g| *g as f32).collect(),
        limiter_active_s: f
            .active
            .iter()
            .map(|a| *a as f64 / f64::from(SAMPLE_RATE))
            .collect(),
        trips: f.trips,
    }
}

/// What the engine says about its stream as it stops (#35): `DriverReleased`
/// once the card is free; a stream that stayed parked (a callback stuck in
/// it, or the parked-engine test's hold) released nothing, so
/// `DriverParked`: the card is free only once the process has ended.
fn stream_end(outcome: StopOutcome, reason: &str) -> EngineMsg {
    let reason = reason.to_owned();
    match outcome {
        StopOutcome::Released => {
            info!("driver released: {reason}");
            EngineMsg::DriverReleased { reason }
        }
        StopOutcome::Parked => {
            error!(
                "the stream stayed parked ({reason}): the driver is not released, \
                 the card is free once this process has ended"
            );
            EngineMsg::DriverParked { reason }
        }
    }
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

    fn drop_peer(&mut self, id: u64, why: &str) {
        if let Some(p) = self.peers.remove(&id) {
            info!("connection {id} closed: {why}");
            p.conn.close();
        }
        if self.controller == Some(id) {
            self.controller = None;
            self.controller_lost = Some(Instant::now());
            warn!(
                "the controller left; solos clear in {:?}",
                self.settings.solo_grace
            );
        }
    }

    /// A peer that takes nothing for `pipe::SEND_TIMEOUT` fails the write
    /// (`Conn::writer`) and is dropped, so a stalled client never holds the
    /// control thread for longer.
    fn write(&mut self, id: u64, bytes: &[u8]) {
        let failed = match self.peers.get(&id) {
            Some(p) => {
                let mut w = p.conn.writer();
                w.write_all(bytes).and_then(|()| w.flush()).err()
            }
            None => return,
        };
        if let Some(e) = failed {
            self.drop_peer(id, &format!("write failed: {e}"));
        }
    }

    fn send(&mut self, id: u64, msg: &EngineMsg) {
        if let Some(bytes) = encode(msg) {
            self.write(id, &bytes);
        }
    }

    /// To every connection that said hello.
    fn broadcast(&mut self, msg: &EngineMsg) {
        let Some(bytes) = encode(msg) else {
            return;
        };
        let ids: Vec<u64> = self
            .peers
            .iter()
            .filter(|(_, p)| p.role.is_some())
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.write(id, &bytes);
        }
    }

    fn reply(&mut self, id: u64, request: u64, error: Option<ErrorBody>) {
        let msg = EngineMsg::Reply(Reply {
            id: request,
            rev: self.core.rev(),
            error,
        });
        self.send(id, &msg);
    }

    fn alarm(&mut self, code: AlarmCode, detail: String) {
        warn!("alarm {code:?}: {detail}");
        let alarm = Alarm { code, detail };
        push_alarm(&mut self.alarms, alarm.clone());
        self.broadcast(&EngineMsg::Alarm(alarm));
    }

    fn state_msg(&self) -> EngineMsg {
        EngineMsg::State {
            rev: self.core.rev(),
            state: self.core.state(),
            transient: self.core.transient(),
        }
    }

    fn hello(&mut self, id: u64, proto: u16, role: Role, client: &str) {
        let Some(proto) = negotiate(PROTO, proto) else {
            let err = ErrorBody {
                code: ErrCode::Unsupported,
                msg: format!(
                    "protocol {proto} is not supported (the engine speaks {PROTO} and one version older)"
                ),
            };
            self.reply(id, 0, Some(err));
            self.drop_peer(id, "unsupported protocol");
            return;
        };
        if role == Role::Control
            && let Some(old) = self.controller.filter(|old| *old != id)
        {
            self.send(old, &EngineMsg::Superseded);
            self.drop_peer(old, "superseded by a new controller");
        }
        if role == Role::Control {
            self.controller = Some(id);
            self.controller_lost = None;
        }
        // A new supervisor (a restarted guard) replaces the old one only.
        if role == Role::Supervisor
            && let Some(old) = self.supervisor.filter(|old| *old != id)
        {
            self.send(old, &EngineMsg::Superseded);
            self.drop_peer(old, "superseded by a new supervisor");
        }
        if role == Role::Supervisor {
            self.supervisor = Some(id);
        }
        match self.peers.get_mut(&id) {
            Some(p) => p.role = Some(role),
            None => return,
        }
        info!(
            "connection {id}: hello from {:?} as {role:?}, protocol {proto}",
            client.chars().take(64).collect::<String>()
        );
        let topo = Arc::clone(self.core.topology());
        let hello = EngineMsg::Hello(Hello {
            proto,
            engine_build: engine_build(),
            topology_hash: topo.hash.clone(),
            state_rev: self.core.rev(),
            sample_rate: SAMPLE_RATE,
            block: self.settings.block,
            role,
        });
        self.send(id, &hello);
        self.send(id, &EngineMsg::Topology(topo.info()));
        let state = self.state_msg();
        self.send(id, &state);
        for alarm in self.alarms.clone() {
            self.send(id, &EngineMsg::Alarm(alarm));
        }
    }

    fn frame(&mut self, id: u64, bytes: &[u8]) {
        let Some(role) = self.peers.get(&id).map(|p| p.role) else {
            return;
        };
        let (request, origin, cmd) = match parse_client(bytes) {
            Err((request, err)) => {
                self.reply(id, request.unwrap_or(0), Some(err));
                return;
            }
            Ok(ClientMsg::Hello {
                proto,
                role,
                client,
            }) => {
                self.hello(id, proto, role, &client);
                return;
            }
            Ok(ClientMsg::Request { id: r, origin, cmd }) => (r, origin, cmd),
        };
        let refuse = |code: ErrCode, msg: &str| {
            Some(ErrorBody {
                code,
                msg: msg.into(),
            })
        };
        match role {
            None => {
                return self.reply(id, request, refuse(ErrCode::BadRequest, "say hello first"));
            }
            Some(Role::Observe) if !cmd.is_read_only() => {
                return self.reply(
                    id,
                    request,
                    refuse(ErrCode::NotController, "an observer may only read"),
                );
            }
            Some(Role::Supervisor) if !cmd.supervisor_may() => {
                return self.reply(
                    id,
                    request,
                    refuse(
                        ErrCode::NotController,
                        "the supervisor never changes the mix",
                    ),
                );
            }
            Some(Role::Control) if cmd.is_supervisor() => {
                return self.reply(
                    id,
                    request,
                    refuse(ErrCode::NotSupervisor, "only the supervisor sends this"),
                );
            }
            _ => {}
        }
        let out = match self.core.apply(&cmd) {
            Ok(out) => out,
            Err(e) => return self.reply(id, request, Some(e.into())),
        };
        match &cmd {
            Cmd::StartTestSignal { .. } | Cmd::HilTestSignal { .. } => {
                self.test_deadline = self
                    .core
                    .transient()
                    .test_signal
                    .map(|t| Instant::now() + Duration::from_secs_f64(t.ttl_s));
            }
            Cmd::StopTestSignal => self.test_deadline = None,
            Cmd::Arm => {
                info!("armed (held until now: {})", self.held);
                self.held = false;
            }
            _ => {}
        }
        let effect = out.effect;
        self.reply(id, request, None);
        self.publish(out, origin);
        match effect {
            Effect::None => {}
            Effect::SendState => {
                let state = self.state_msg();
                self.send(id, &state);
            }
            Effect::SendTopology => {
                let info = self.core.topology().info();
                self.send(id, &EngineMsg::Topology(info));
            }
            Effect::Save => self.save(),
            Effect::Shutdown => self.shutdown = true,
            Effect::Reopen => {
                if self.driver.as_ref().is_some_and(|d| d.force_reopen()) {
                    info!("a forced reopen of the card was asked for");
                } else {
                    warn!("a forced reopen was asked for, but the backend has no card");
                }
            }
            Effect::Imported { baseline } => {
                let state = self.state_msg();
                self.broadcast(&state);
                self.schedule.changed(Instant::now());
                if baseline {
                    self.save_baseline();
                }
            }
        }
    }

    /// Queues an outcome's RT commands and broadcasts its changes.
    fn publish(&mut self, out: Outcome, origin: Option<u64>) {
        for chunk in out.rt.chunks(MAX_CMDS_PER_BLOCK) {
            self.pending.push_back(chunk.to_vec());
        }
        self.flush_rt();
        if !out.changes.is_empty() {
            self.schedule.changed(Instant::now());
            self.broadcast(&EngineMsg::Delta {
                rev: out.rev,
                origin,
                changes: out.changes,
            });
        }
    }

    fn flush_rt(&mut self) {
        while let Some(group) = self.pending.front() {
            if !push_group(&mut self.cmds, 0, group) {
                return;
            }
            self.pending.pop_front();
        }
    }

    fn persisted(&self) -> Persisted {
        let topo = self.core.topology();
        Persisted {
            rev: self.core.rev(),
            topology_hash: topo.hash.clone(),
            saved_unix_ms: unix_ms(),
            state: self.core.state(),
            counters: topo
                .mixes
                .iter()
                .zip(&self.counters)
                .map(|(b, c)| (b.id.clone(), *c))
                .collect(),
        }
    }

    fn save(&mut self) {
        self.schedule.saved();
        match self.store.save(&self.persisted()) {
            Ok(committed) => {
                // The save stands; only old generations stayed (#32 P6).
                if let Some(why) = &committed.pruning {
                    warn!("old generations were not removed: {why}");
                }
                // #32 MAJOR-1: kept, never loaded; the engineer hears where.
                if let Some(aside) = &committed.orphaned {
                    self.alarm(
                        AlarmCode::StateFallback,
                        format!(
                            "a save.tmp the boot did not load was moved aside to {}",
                            aside.display()
                        ),
                    );
                }
                let rev = self.core.rev();
                self.broadcast(&EngineMsg::Saved {
                    rev,
                    generation: committed.generation,
                });
            }
            Err(e) => self.alarm(
                AlarmCode::SaveFailed,
                format!("saving the state failed: {e}"),
            ),
        }
    }

    fn save_baseline(&mut self) {
        if let Err(e) = self.store.save_baseline(&self.persisted()) {
            self.alarm(
                AlarmCode::SaveFailed,
                format!("saving the baseline failed: {e}"),
            );
        }
    }

    /// Stops the backend and says how its stream ended, as the last word
    /// before every connection closes.
    fn release(&mut self, reason: &str) {
        let outcome = self
            .driver
            .take()
            .map_or(StopOutcome::Released, |d| d.stop());
        self.broadcast(&stream_end(outcome, reason));
        for (_, p) in std::mem::take(&mut self.peers) {
            p.conn.close();
        }
    }

    fn shutdown_now(&mut self) -> Exit {
        info!("shutdown: saving, fading out, releasing the driver");
        self.save();
        let faded = self.fade_out();
        self.release("shutdown");
        Exit::Shutdown { faded }
    }

    /// Asks the callback to fade out and waits for it, at most `FADE_WAIT`;
    /// whether it faded.
    fn fade_out(&mut self) -> bool {
        self.pending.push_back(vec![RtOp::FadeOut]);
        let t0 = Instant::now();
        while !self.status.faded_out.load(Ordering::Acquire) && t0.elapsed() < FADE_WAIT {
            self.flush_rt();
            std::thread::sleep(Duration::from_millis(5));
        }
        self.status.faded_out.load(Ordering::Acquire)
    }

    fn fault(&mut self, why: String) -> Exit {
        error!("the RT callback faulted: {why}");
        self.save();
        self.alarm(AlarmCode::Fault, why.clone());
        self.release("fault");
        Exit::Fault(why)
    }

    /// The backend refused the card: save, stop without a fade (the card may
    /// be in a wrong state), exit 3 — the guard never respawns after it.
    fn card(&mut self, why: String) -> Exit {
        error!("the card is refused: {why}");
        self.save();
        self.release("card refused");
        Exit::Card(why)
    }

    fn status_msg(&self, st: &StreamStats) -> Status {
        let h = self
            .driver
            .as_ref()
            .and_then(|d| d.histograms())
            .unwrap_or_default();
        Status {
            callbacks: st.callbacks,
            late: st.late,
            faulted: st.faulted,
            process_max_us: st.max_process_ns as f64 / 1000.0,
            trips: self.status.trips.load(Ordering::Relaxed),
            tap_overruns: self.status.tap_overruns.load(Ordering::Relaxed),
            talkback_dropped: self.talkback_dropped.load(Ordering::Relaxed),
            cmd_backlog: self.pending.iter().map(|g| g.len() as u64).sum(),
            frames: st.frames,
            missed: st.missed,
            overruns: st.overruns,
            resets: st.resets,
            parked: st.parked,
            held: self.held,
            lock_failed: self.driver.as_ref().is_some_and(|d| d.lock_failed()),
            hil: self
                .core
                .hil()
                .iter()
                .zip(&self.hil_peaks)
                .map(|(&tx, &peak)| HilOut {
                    tx,
                    peak: peak as f32,
                })
                .collect(),
            loopback_samples: self.status.loopback_samples.load(Ordering::Relaxed),
            interval_hist: h.interval,
            process_hist: h.process,
            hist_top_us: h.top_us,
            last_reopen_us: st.last_reopen_us,
            fault_callback_us: st.fault_callback_ns as f64 / 1000.0,
        }
    }

    /// A meter frame's peaks of HIL's spare outputs join those since the
    /// last `Status` (S6).
    fn note_hil(&mut self, peaks: &[f64]) {
        for (held, &p) in self.hil_peaks.iter_mut().zip(peaks) {
            *held = held.max(p);
        }
    }

    /// The `Status` to broadcast now; HIL's peaks start again after it.
    fn next_status(&mut self, st: &StreamStats) -> Status {
        let status = self.status_msg(st);
        self.hil_peaks.fill(0.0);
        status
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
