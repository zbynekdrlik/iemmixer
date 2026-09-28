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
    fn stop(self: Box<Self>);
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
            Ok(generation) => {
                let rev = self.core.rev();
                self.broadcast(&EngineMsg::Saved { rev, generation });
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

    fn release(&mut self, reason: &str) {
        if let Some(d) = self.driver.take() {
            d.stop();
        }
        info!("driver released: {reason}");
        self.broadcast(&EngineMsg::DriverReleased {
            reason: reason.into(),
        });
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
mod tests {
    use super::*;

    fn alarm(k: usize) -> Alarm {
        Alarm {
            code: AlarmCode::Sanitizer,
            detail: k.to_string(),
        }
    }

    #[test]
    fn alarms_keep_the_newest_sixteen() {
        let mut list = Vec::new();
        for k in 0..MAX_ALARMS {
            push_alarm(&mut list, alarm(k));
        }
        assert_eq!(list.len(), MAX_ALARMS);
        assert_eq!(list[0].detail, "0");
        push_alarm(&mut list, alarm(16));
        assert_eq!(list.len(), MAX_ALARMS);
        assert_eq!(
            (list[0].detail.as_str(), list[15].detail.as_str()),
            ("1", "16")
        );
    }

    #[test]
    fn meter_frames_convert_to_the_protocol() {
        let f = MeterFrame {
            seq: 3,
            inputs: vec![[0.5, 0.25]],
            mixes: vec![[1.0, 0.0], [0.125, 2.0]],
            groups: vec![[0.5, 0.0], [0.0, 0.75]],
            gr_db: vec![-3.0, 0.0],
            active: vec![96_000, 48_000],
            trips: 2,
            hil: vec![0.25],
        };
        let m = meters_msg(&f);
        assert_eq!(m.seq, 3);
        assert_eq!(m.inputs, vec![[0.5f32, 0.25]]);
        assert_eq!(m.mixes, vec![[1.0f32, 0.0], [0.125, 2.0]]);
        assert_eq!(m.groups, vec![[0.5f32, 0.0], [0.0, 0.75]]);
        assert_eq!(m.gr_db, vec![-3.0f32, 0.0]);
        assert_eq!(m.limiter_active_s, vec![1.0, 0.5]);
        assert_eq!(m.trips, 2);
    }

    #[test]
    fn the_build_names_the_version() {
        assert!(engine_build().starts_with(concat!(env!("CARGO_PKG_VERSION"), "+")));
        assert!(unix_ms() > 1_700_000_000_000);
    }

    /// A backend that runs and never faults.
    struct Idle;

    impl Driver for Idle {
        fn stats(&self) -> StreamStats {
            StreamStats {
                running: true,
                ..StreamStats::default()
            }
        }

        fn stop(self: Box<Self>) {}
    }

    struct Rig {
        c: Control,
        meters: triple_buffer::Input<MeterFrame>,
        status: Arc<RtStatus>,
        _ring: rtrb::Consumer<RtCmd>,
        dir: tempfile::TempDir,
    }

    /// A control loop on the test site with the processor's ends in hand.
    fn rig() -> Rig {
        let flags = crate::core::Flags {
            test_signal: true,
            fault_injection: false,
        };
        rig_with(flags, false)
    }

    /// HIL's spare outputs of the test site (`[guard] hil_tx`).
    const SPARE: [u16; 2] = [94, 95];

    /// Like `run`, the engine opens HIL's spare outputs under the
    /// test-signal flag only.
    fn rig_with(flags: crate::core::Flags, hold: bool) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let topo = Arc::new(crate::test_support::test_site());
        let hil = if flags.test_signal {
            SPARE.to_vec()
        } else {
            Vec::new()
        };
        let core = Core::new(
            Arc::clone(&topo),
            &iem_engine_proto::MixState::default(),
            0,
            flags,
        )
        .with_hil(hil);
        let (cmds, ring) = rtrb::RingBuffer::new(crate::rt::CMD_RING);
        let (meters_in, meters) = triple_buffer::triple_buffer(&MeterFrame::default());
        let status = Arc::new(RtStatus::default());
        let c = Control::new(Parts {
            core,
            store: Store::open(&dir.path().join("state")).unwrap(),
            cmds,
            meters,
            status: Arc::clone(&status),
            talkback_dropped: Arc::new(AtomicU64::new(0)),
            driver: Box::new(Idle),
            counters: vec![0; topo.mixes.len()],
            alarms: Vec::new(),
            settings: Settings {
                solo_grace: Duration::from_secs(10),
                block: 32,
                hold,
            },
        });
        Rig {
            c,
            meters: meters_in,
            status,
            _ring: ring,
            dir,
        }
    }

    /// `run` on a thread: its exit and how long it took; fails after 5 s
    /// instead of hanging.
    fn run_bounded(c: Control, rx: Receiver<CtlMsg>) -> (Exit, Duration) {
        let (tx, done) = std::sync::mpsc::channel();
        let _ = std::thread::spawn(move || {
            let t0 = Instant::now();
            let exit = c.run(&rx);
            let _ = tx.send((exit, t0.elapsed()));
        });
        done.recv_timeout(Duration::from_secs(5))
            .expect("run returns")
    }

    #[test]
    fn shutdown_waits_for_the_fade_but_not_beyond_it() {
        // Already faded: saved, and the exit says so.
        let mut r = rig();
        r.status.faded_out.store(true, Ordering::Release);
        r.c.shutdown = true;
        let (_tx, rx) = std::sync::mpsc::channel();
        let (exit, _) = run_bounded(r.c, rx);
        assert_eq!(exit, Exit::Shutdown { faded: true });
        assert!(r.dir.path().join("state/current.json").exists(), "saved");
        // Never faded: the driver is released after FADE_WAIT.
        let mut r = rig();
        r.c.shutdown = true;
        let (_tx, rx) = std::sync::mpsc::channel();
        let (exit, took) = run_bounded(r.c, rx);
        assert_eq!(exit, Exit::Shutdown { faded: false });
        assert!(took >= FADE_WAIT, "{took:?}");
    }

    #[test]
    fn the_fade_wait_ends_at_the_fade_or_after_fade_wait() {
        // Timed without the state save (its file I/O is slow on some hosts).
        let mut r = rig();
        r.status.faded_out.store(true, Ordering::Release);
        let t0 = Instant::now();
        assert!(r.c.fade_out());
        assert!(t0.elapsed() < FADE_WAIT / 2, "{:?}", t0.elapsed());
        let mut r = rig();
        let t0 = Instant::now();
        assert!(!r.c.fade_out());
        assert!(t0.elapsed() >= FADE_WAIT, "{:?}", t0.elapsed());
    }

    #[test]
    fn status_reports_microseconds_counters_and_the_backlog() {
        let mut r = rig();
        r.c.pending.push_back(vec![RtOp::Nop; 3]);
        r.c.pending.push_back(vec![RtOp::Nop; 2]);
        r.status.trips.store(4, Ordering::Relaxed);
        let st = StreamStats {
            frames: 32,
            callbacks: 7,
            late: 1,
            missed: 0,
            overruns: 0,
            resets: 0,
            parked: false,
            faulted: false,
            running: true,
            max_process_ns: 2_500_000,
            fault: None,
        };
        let s = r.c.status_msg(&st);
        assert_eq!((s.callbacks, s.late, s.faulted), (7, 1, false));
        assert_eq!(s.process_max_us, 2500.0);
        assert_eq!((s.trips, s.cmd_backlog), (4, 5));
    }

    #[test]
    fn status_carries_the_measured_period_the_stream_counters_and_the_hold() {
        let r = rig_with(crate::core::Flags::default(), true);
        let st = StreamStats {
            frames: 32,
            callbacks: 7,
            missed: 2,
            overruns: 3,
            resets: 4,
            parked: true,
            running: true,
            ..StreamStats::default()
        };
        let s = r.c.status_msg(&st);
        assert_eq!((s.frames, s.missed, s.overruns, s.resets), (32, 2, 3, 4));
        assert!(s.parked, "parked");
        assert!(s.held, "held until Arm");
        assert!(!s.lock_failed);
        let s = rig().c.status_msg(&StreamStats {
            frames: 64,
            ..StreamStats::default()
        });
        assert_eq!((s.frames, s.missed, s.overruns, s.resets), (64, 0, 0, 0));
        assert!(!s.parked && !s.held);
    }

    /// HIL v1 proves the test signal from the engine (S6): each `Status`
    /// carries every spare output's peak since the previous one, the
    /// largest of the meter frames between them, which starts again after
    /// it; an engine without spare outputs lists none.
    #[test]
    fn status_carries_the_hil_outputs_peaks_since_the_previous_status() {
        let mut r = rig();
        let st = StreamStats::default();
        let out = |tx: u16, peak: f32| HilOut { tx, peak };
        assert_eq!(r.c.status_msg(&st).hil, vec![out(94, 0.0), out(95, 0.0)]);
        r.c.note_hil(&[0.01, 0.0]);
        r.c.note_hil(&[0.03, 0.02]);
        r.c.note_hil(&[0.02, 0.01]);
        r.c.note_hil(&[0.5]);
        assert_eq!(r.c.next_status(&st).hil, vec![out(94, 0.5), out(95, 0.02)]);
        assert_eq!(r.c.next_status(&st).hil, vec![out(94, 0.0), out(95, 0.0)]);
        // The control loop feeds them from each meter frame it reads and
        // starts again after each Status it sends.
        let t0 = Instant::now();
        r.meters.write(MeterFrame {
            hil: vec![0.04, 0.001],
            ..MeterFrame::default()
        });
        assert!(r.c.tick(t0).is_none());
        assert_eq!(r.c.hil_peaks, [0.04, 0.001]);
        assert!(r.c.tick(t0 + Duration::from_secs(2)).is_none());
        assert_eq!(r.c.hil_peaks, [0.0, 0.0]);
        let quiet = rig_with(crate::core::Flags::default(), false);
        assert!(quiet.c.status_msg(&st).hil.is_empty());
        assert!(quiet.c.hil_peaks.is_empty());
    }

    /// A backend with scripted hooks: its ticks and forced reopens counted,
    /// an ending, a lock failure.
    struct Scripted {
        ticks: Arc<AtomicU64>,
        ending: Option<Ending>,
        lock_failed: bool,
    }

    impl Driver for Scripted {
        fn stats(&self) -> StreamStats {
            StreamStats {
                running: true,
                ..StreamStats::default()
            }
        }

        fn stop(self: Box<Self>) {}

        fn tick(&mut self, _now: Instant) {
            self.ticks.fetch_add(1, Ordering::Relaxed);
        }

        fn ending(&self) -> Option<Ending> {
            self.ending.clone()
        }

        fn lock_failed(&self) -> bool {
            self.lock_failed
        }

        /// Counted with the ticks, [`REOPEN`] apiece, so one counter shows
        /// both.
        fn force_reopen(&self) -> bool {
            self.ticks.fetch_add(REOPEN, Ordering::Relaxed);
            true
        }
    }

    /// What one forced reopen adds to a scripted backend's counter.
    const REOPEN: u64 = 1 << 32;

    fn scripted(ending: Option<Ending>, lock_failed: bool) -> (Box<dyn Driver>, Arc<AtomicU64>) {
        let ticks = Arc::new(AtomicU64::new(0));
        let d = Scripted {
            ticks: Arc::clone(&ticks),
            ending,
            lock_failed,
        };
        (Box::new(d), ticks)
    }

    #[test]
    fn the_backend_is_ticked_and_its_lock_failure_is_reported() {
        let mut r = rig();
        let (d, ticks) = scripted(None, true);
        r.c.driver = Some(d);
        assert!(r.c.tick(Instant::now()).is_none());
        assert!(r.c.tick(Instant::now()).is_none());
        assert_eq!(ticks.load(Ordering::Relaxed), 2);
        assert!(r.c.status_msg(&StreamStats::default()).lock_failed);
        // NullRt (and any backend that does not say otherwise) locks nothing.
        assert!(!rig().c.status_msg(&StreamStats::default()).lock_failed);
        assert!(!Idle.lock_failed());
        assert_eq!(Idle.ending(), None);
    }

    #[test]
    fn a_session_end_takes_the_shutdown_path() {
        let mut r = rig();
        r.status.faded_out.store(true, Ordering::Release);
        r.c.driver = Some(scripted(Some(Ending::Session), false).0);
        assert_eq!(
            r.c.tick(Instant::now()),
            Some(Exit::Shutdown { faded: true })
        );
        assert!(r.dir.path().join("state/current.json").exists(), "saved");
        assert!(r.c.driver.is_none(), "released");
    }

    #[test]
    fn a_card_refused_while_running_ends_with_exit_3_and_no_fade() {
        let mut r = rig();
        let why = "the preferred buffer was not restored";
        r.c.driver = Some(scripted(Some(Ending::Card(why.into())), false).0);
        let t0 = Instant::now();
        assert_eq!(r.c.tick(Instant::now()), Some(Exit::Card(why.into())));
        assert!(t0.elapsed() < FADE_WAIT, "{:?}", t0.elapsed());
        assert!(r.dir.path().join("state/current.json").exists(), "saved");
        assert!(r.c.driver.is_none(), "released");
    }

    #[test]
    fn only_new_sanitiser_trips_raise_an_alarm() {
        let mut r = rig();
        let now = Instant::now();
        for (trips, alarms) in [(0, 0), (2, 1), (2, 1), (3, 2)] {
            r.meters.input_buffer_mut().trips = trips;
            r.meters.publish();
            assert!(r.c.tick(now).is_none());
            assert_eq!(r.c.alarms.len(), alarms, "at {trips} trips");
        }
        assert_eq!(r.c.alarms[0].code, AlarmCode::Sanitizer);
        assert_eq!(r.c.alarms[0].detail, "sanitiser trips: 2");
        assert_eq!(r.c.alarms[1].detail, "sanitiser trips: 3");
    }

    /// Connections need a socket pair. Unix only: the harness's client
    /// reads with a socket receive timeout, which Windows pipes do not have;
    /// `tests/pipes.rs` runs the engine's pipes on Windows.
    #[cfg(unix)]
    mod peers {
        use super::*;
        use crate::pipe::{control_name, listen};
        use iem_engine_proto::{InputId, MixId, MixState, read_frame};
        use interprocess::local_socket::Stream;
        use interprocess::local_socket::prelude::*;

        const WAIT: Duration = Duration::from_secs(5);

        /// The engine's end and the client's end of one connection.
        fn peer(dir: &std::path::Path) -> (Conn, Stream) {
            peer_named(dir, "ctl")
        }

        /// One connection through its own socket `<name>.sock`.
        fn peer_named(dir: &std::path::Path, name: &str) -> (Conn, Stream) {
            let path = dir
                .join(format!("{name}.sock"))
                .to_string_lossy()
                .into_owned();
            let listener = listen(control_name(&path).unwrap()).unwrap();
            let client = Stream::connect(control_name(&path).unwrap()).unwrap();
            client.set_recv_timeout(Some(WAIT)).unwrap();
            let start = Instant::now();
            loop {
                match listener.accept() {
                    Ok(s) => return (Conn::new(s), client),
                    Err(e) => {
                        assert!(start.elapsed() < WAIT, "accept: {e}");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
            }
        }

        /// Reads every message until the engine closes the connection (or 5 s pass).
        fn reader(mut client: Stream) -> std::thread::JoinHandle<Vec<EngineMsg>> {
            std::thread::spawn(move || {
                let mut out = Vec::new();
                let mut buf = Vec::new();
                while read_frame(&mut client, &mut buf).is_ok() {
                    out.push(serde_json::from_slice(&buf).unwrap());
                }
                out
            })
        }

        fn frame(msg: &ClientMsg) -> CtlMsg {
            CtlMsg::Frame {
                id: 1,
                bytes: serde_json::to_vec(msg).unwrap(),
            }
        }

        fn hello() -> CtlMsg {
            frame(&ClientMsg::Hello {
                proto: PROTO,
                role: Role::Control,
                client: "test".into(),
            })
        }

        fn request(id: u64, cmd: Cmd) -> CtlMsg {
            frame(&ClientMsg::Request {
                id,
                origin: None,
                cmd,
            })
        }

        /// A frame from connection `conn`.
        fn frame_from(conn: u64, msg: &ClientMsg) -> CtlMsg {
            CtlMsg::Frame {
                id: conn,
                bytes: serde_json::to_vec(msg).unwrap(),
            }
        }

        fn hello_as(conn: u64, role: Role) -> CtlMsg {
            frame_from(
                conn,
                &ClientMsg::Hello {
                    proto: PROTO,
                    role,
                    client: "test".into(),
                },
            )
        }

        fn request_from(conn: u64, id: u64, cmd: Cmd) -> CtlMsg {
            frame_from(
                conn,
                &ClientMsg::Request {
                    id,
                    origin: None,
                    cmd,
                },
            )
        }

        /// The error code of every reply (None: accepted), by request id.
        fn codes(msgs: &[EngineMsg]) -> BTreeMap<u64, Option<ErrCode>> {
            msgs.iter()
                .filter_map(|m| match m {
                    EngineMsg::Reply(r) => Some((r.id, r.error.as_ref().map(|e| e.code))),
                    _ => None,
                })
                .collect()
        }

        fn superseded(msgs: &[EngineMsg]) -> usize {
            msgs.iter()
                .filter(|m| matches!(m, EngineMsg::Superseded))
                .count()
        }

        /// The HIL signal on a spare output: never a mix's TX (the owner's
        /// decision on #9 of 2026-09-28).
        fn hil() -> Cmd {
            Cmd::HilTestSignal {
                input: InputId::new("mic1"),
                hz: 1000.0,
                dbfs: -30.0,
                ttl_s: 60.0,
                card_tx: vec![SPARE[1]],
            }
        }

        fn set_mix() -> Cmd {
            Cmd::SetMix {
                mix: MixId::new("member1"),
                volume_db: Some(-3.0),
                muted: None,
            }
        }

        #[test]
        fn the_supervisor_may_stop_save_arm_and_test_but_never_mix() {
            let mut r = rig_with(
                crate::core::Flags {
                    test_signal: true,
                    fault_injection: false,
                },
                true,
            );
            let (ctl, ctl_client) = peer_named(r.dir.path(), "ctl");
            let (sup, sup_client) = peer_named(r.dir.path(), "sup");
            let (obs, obs_client) = peer_named(r.dir.path(), "obs");
            let (ctl_got, sup_got, obs_got) =
                (reader(ctl_client), reader(sup_client), reader(obs_client));
            for (id, conn) in [(1, ctl), (2, sup), (3, obs)] {
                r.c.handle(CtlMsg::Connected { id, conn });
            }
            r.c.handle(hello_as(1, Role::Control));
            r.c.handle(hello_as(2, Role::Supervisor));
            r.c.handle(hello_as(3, Role::Observe));
            // The supervisor's own commands, the reads, the save and the test
            // signals (under their flags); never a mix change.
            r.c.handle(request_from(2, 10, hil()));
            assert!(r.c.test_deadline.is_some(), "the HIL signal ends by itself");
            assert!(r.c.status_msg(&StreamStats::default()).held);
            r.c.handle(request_from(2, 11, Cmd::Arm));
            assert!(!r.c.status_msg(&StreamStats::default()).held, "armed");
            let start = Cmd::StartTestSignal {
                input: InputId::new("mic2"),
                hz: 500.0,
                dbfs: -40.0,
                ttl_s: 1.0,
            };
            let supervisor: [(u64, Cmd, Option<ErrCode>); 10] = [
                (12, Cmd::Ping, None),
                (13, Cmd::GetState, None),
                (14, Cmd::GetTopology, None),
                (15, Cmd::SaveNow, None),
                // A running HIL signal refuses a plain one: stop it first.
                (16, Cmd::StopTestSignal, None),
                (17, start, None),
                (18, Cmd::InjectFault, Some(ErrCode::Forbidden)),
                (19, set_mix(), Some(ErrCode::NotController)),
                (
                    20,
                    Cmd::Batch {
                        ops: vec![set_mix()],
                    },
                    Some(ErrCode::NotController),
                ),
                (
                    21,
                    Cmd::ImportState {
                        state: MixState::default(),
                        baseline: false,
                    },
                    Some(ErrCode::NotController),
                ),
            ];
            for (id, cmd, _) in &supervisor {
                r.c.handle(request_from(2, *id, cmd.clone()));
            }
            // The controller may not arm or start the HIL signal; an observer
            // may do neither either.
            r.c.handle(request_from(1, 30, Cmd::Arm));
            r.c.handle(request_from(1, 31, hil()));
            r.c.handle(request_from(1, 32, set_mix()));
            r.c.handle(request_from(3, 40, Cmd::Arm));
            r.c.handle(request_from(3, 41, hil()));
            // The supervisor reads the meters like everyone.
            r.meters.input_buffer_mut().seq = 5;
            r.meters.publish();
            assert!(r.c.tick(Instant::now()).is_none());
            r.c.handle(request_from(2, 50, Cmd::Shutdown));
            assert!(r.c.shutdown, "the supervisor stops the engine");
            drop(r);
            let sup = sup_got.join().unwrap();
            assert!(
                matches!(&sup[0], EngineMsg::Hello(h) if h.role == Role::Supervisor),
                "{:?}",
                sup[0]
            );
            let got = codes(&sup);
            assert_eq!((got[&10], got[&11], got[&50]), (None, None, None));
            for (id, cmd, want) in &supervisor {
                assert_eq!(got[id], *want, "{cmd:?}");
            }
            assert!(
                sup.iter()
                    .any(|m| matches!(m, EngineMsg::Meters(f) if f.seq == 5)),
                "meters"
            );
            let ctl = codes(&ctl_got.join().unwrap());
            assert_eq!(
                (ctl[&30], ctl[&31], ctl[&32]),
                (
                    Some(ErrCode::NotSupervisor),
                    Some(ErrCode::NotSupervisor),
                    None
                )
            );
            let obs = codes(&obs_got.join().unwrap());
            assert_eq!(
                (obs[&40], obs[&41]),
                (Some(ErrCode::NotController), Some(ErrCode::NotController))
            );
        }

        /// HIL's forced reopen (S6 design note §7): the supervisor's, under
        /// the fault-injection flag, reaches the backend; the controller's is
        /// refused, and so is one without the flag.
        #[test]
        fn a_forced_reopen_reaches_the_backend_from_the_supervisor_only() {
            for (fault_injection, reopens, sup_code) in
                [(true, REOPEN, None), (false, 0, Some(ErrCode::Forbidden))]
            {
                let mut r = rig_with(
                    crate::core::Flags {
                        test_signal: false,
                        fault_injection,
                    },
                    false,
                );
                let (d, count) = scripted(None, false);
                r.c.driver = Some(d);
                let (ctl, ctl_client) = peer_named(r.dir.path(), "ctl");
                let (sup, sup_client) = peer_named(r.dir.path(), "sup");
                let (ctl_got, sup_got) = (reader(ctl_client), reader(sup_client));
                r.c.handle(CtlMsg::Connected { id: 1, conn: ctl });
                r.c.handle(CtlMsg::Connected { id: 2, conn: sup });
                r.c.handle(hello_as(1, Role::Control));
                r.c.handle(hello_as(2, Role::Supervisor));
                r.c.handle(request_from(1, 7, Cmd::ForceReopen));
                assert_eq!(count.load(Ordering::Relaxed), 0, "the controller's");
                r.c.handle(request_from(2, 8, Cmd::ForceReopen));
                assert_eq!(count.load(Ordering::Relaxed), reopens);
                drop(r);
                let ctl = codes(&ctl_got.join().unwrap());
                assert_eq!(ctl[&7], Some(ErrCode::NotSupervisor));
                let sup = codes(&sup_got.join().unwrap());
                assert_eq!(sup[&8], sup_code, "fault injection {fault_injection}");
            }
            // NullRt has no card: the request is answered, nothing reopens.
            assert!(!Idle.force_reopen());
        }

        #[test]
        fn the_test_signals_stay_under_their_flag_for_the_supervisor() {
            let mut r = rig_with(crate::core::Flags::default(), false);
            let (sup, sup_client) = peer_named(r.dir.path(), "sup");
            let got = reader(sup_client);
            r.c.handle(CtlMsg::Connected { id: 2, conn: sup });
            r.c.handle(hello_as(2, Role::Supervisor));
            r.c.handle(request_from(2, 1, hil()));
            r.c.handle(request_from(
                2,
                2,
                Cmd::StartTestSignal {
                    input: InputId::new("mic2"),
                    hz: 500.0,
                    dbfs: -40.0,
                    ttl_s: 1.0,
                },
            ));
            assert_eq!(r.c.test_deadline, None);
            drop(r);
            let got = codes(&got.join().unwrap());
            assert_eq!(
                (got[&1], got[&2]),
                (Some(ErrCode::Forbidden), Some(ErrCode::Forbidden))
            );
        }

        #[test]
        fn a_second_supervisor_replaces_the_first_and_leaves_the_controller() {
            let mut r = rig();
            let mut clients = Vec::new();
            for (id, name) in [(1, "c1"), (2, "s1"), (3, "s2"), (4, "c2")] {
                let (conn, client) = peer_named(r.dir.path(), name);
                clients.push(reader(client));
                r.c.handle(CtlMsg::Connected { id, conn });
            }
            r.c.handle(hello_as(1, Role::Control));
            r.c.handle(hello_as(2, Role::Supervisor));
            r.c.handle(hello_as(3, Role::Supervisor));
            assert!(!r.c.peers.contains_key(&2), "the first supervisor is gone");
            assert!(r.c.peers.contains_key(&3));
            assert_eq!(r.c.controller, Some(1), "the controller stays");
            r.c.handle(request_from(1, 5, set_mix()));
            r.c.handle(request_from(3, 6, Cmd::Arm));
            // A new controller leaves the supervisor alone.
            r.c.handle(hello_as(4, Role::Control));
            assert_eq!(r.c.controller, Some(4));
            assert!(r.c.peers.contains_key(&3), "the supervisor stays");
            assert!(!r.c.peers.contains_key(&1));
            r.c.handle(request_from(3, 7, Cmd::Ping));
            drop(r);
            let got: Vec<Vec<EngineMsg>> = clients.into_iter().map(|c| c.join().unwrap()).collect();
            let [c1, s1, s2, c2] = [&got[0], &got[1], &got[2], &got[3]];
            assert_eq!(superseded(s1), 1);
            assert_eq!(superseded(s2), 0);
            assert_eq!(superseded(c1), 1, "by the second controller only");
            assert_eq!(superseded(c2), 0);
            assert_eq!(codes(c1)[&5], None, "set before the new controller");
            let s2 = codes(s2);
            assert_eq!((s2[&6], s2[&7]), (None, None));
        }

        #[test]
        fn nothing_after_a_shutdown_request_is_handled() {
            let r = rig();
            r.status.faded_out.store(true, Ordering::Release);
            let (conn, client) = peer(r.dir.path());
            let got = reader(client);
            let (tx, rx) = std::sync::mpsc::channel();
            for msg in [
                CtlMsg::Connected { id: 1, conn },
                hello(),
                request(2, Cmd::Shutdown),
                request(3, Cmd::Ping),
            ] {
                tx.send(msg).unwrap();
            }
            let (exit, _) = run_bounded(r.c, rx);
            assert_eq!(exit, Exit::Shutdown { faded: true });
            let replies: Vec<u64> = got
                .join()
                .unwrap()
                .iter()
                .filter_map(|m| match m {
                    EngineMsg::Reply(rep) => Some(rep.id),
                    _ => None,
                })
                .collect();
            assert_eq!(
                replies,
                vec![2],
                "the ping after the shutdown stays unanswered"
            );
        }

        #[test]
        fn stopping_the_test_signal_clears_its_deadline() {
            let mut r = rig();
            let (conn, client) = peer(r.dir.path());
            let _got = reader(client);
            r.c.handle(CtlMsg::Connected { id: 1, conn });
            r.c.handle(hello());
            r.c.handle(request(
                2,
                Cmd::StartTestSignal {
                    input: InputId::new("mic1"),
                    hz: 1000.0,
                    dbfs: -30.0,
                    ttl_s: 60.0,
                },
            ));
            assert!(r.c.test_deadline.is_some());
            r.c.handle(request(3, Cmd::StopTestSignal));
            assert_eq!(r.c.test_deadline, None);
            assert!(r.c.core.transient().test_signal.is_none());
        }

        #[test]
        fn an_import_as_baseline_writes_the_baseline() {
            let mut r = rig();
            let (conn, client) = peer(r.dir.path());
            let _got = reader(client);
            r.c.handle(CtlMsg::Connected { id: 1, conn });
            r.c.handle(hello());
            let import = |baseline: bool| Cmd::ImportState {
                state: MixState::default(),
                baseline,
            };
            let baseline = r.dir.path().join("state/baseline.json");
            r.c.handle(request(2, import(false)));
            assert_eq!(r.c.core.rev(), 1);
            assert!(!baseline.exists(), "a plain import keeps the baseline");
            r.c.handle(request(3, import(true)));
            let saved = crate::persist::decode(&std::fs::read(&baseline).unwrap()).unwrap();
            assert_eq!((saved.rev, r.c.core.rev()), (2, 2));
            assert_eq!(saved.topology_hash, r.c.core.topology().hash);
        }
    }
}
