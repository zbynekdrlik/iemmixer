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
    Alarm, AlarmCode, ClientMsg, Cmd, EngineMsg, ErrCode, ErrorBody, Hello, Meters, PROTO, Reply,
    Role, Status, negotiate, parse_client, write_frame,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// X2: solos clear this long after the controller left.
    pub solo_grace: Duration,
    pub block: u32,
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
    test_deadline: Option<Instant>,
    counters: Vec<u64>,
    alarms: Vec<Alarm>,
    last_status: Instant,
    settings: Settings,
    shutdown: bool,
    /// Sanitiser trips already alarmed.
    trips_seen: u64,
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
        buses: f.buses.iter().map(pair).collect(),
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
            test_deadline: None,
            counters: p.counters,
            alarms: p.alarms,
            last_status: Instant::now(),
            settings: p.settings,
            shutdown: false,
            trips_seen: 0,
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

    fn write(&mut self, id: u64, bytes: &[u8]) {
        let failed = match self.peers.get(&id) {
            Some(p) => (&*p.conn.stream)
                .write_all(bytes)
                .and_then(|()| (&*p.conn.stream).flush())
                .err(),
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
        match self.peers.get_mut(&id) {
            Some(p) => p.role = Some(role),
            None => return,
        }
        info!(
            "connection {id}: hello from {:?} as {role:?}, protocol {proto}",
            client.chars().take(64).collect::<String>()
        );
        let graph = Arc::clone(self.core.graph());
        let hello = EngineMsg::Hello(Hello {
            proto,
            engine_build: engine_build(),
            topology_hash: graph.hash.clone(),
            state_rev: self.core.rev(),
            sample_rate: SAMPLE_RATE,
            block: self.settings.block,
            role,
        });
        self.send(id, &hello);
        self.send(id, &EngineMsg::Topology(graph.info()));
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
            _ => {}
        }
        let out = match self.core.apply(&cmd) {
            Ok(out) => out,
            Err(e) => return self.reply(id, request, Some(e.into())),
        };
        match &cmd {
            Cmd::StartTestSignal { .. } => {
                self.test_deadline = self
                    .core
                    .transient()
                    .test_signal
                    .map(|t| Instant::now() + Duration::from_secs_f64(t.ttl_s));
            }
            Cmd::StopTestSignal => self.test_deadline = None,
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
                let info = self.core.graph().info();
                self.send(id, &EngineMsg::Topology(info));
            }
            Effect::Save => self.save(),
            Effect::Shutdown => self.shutdown = true,
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
        let graph = self.core.graph();
        Persisted {
            rev: self.core.rev(),
            topology_hash: graph.hash.clone(),
            saved_unix_ms: unix_ms(),
            state: self.core.state(),
            counters: graph
                .buses
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
        self.pending.push_back(vec![RtOp::FadeOut]);
        let t0 = Instant::now();
        while !self.status.faded_out.load(Ordering::Acquire) && t0.elapsed() < FADE_WAIT {
            self.flush_rt();
            std::thread::sleep(Duration::from_millis(5));
        }
        let faded = self.status.faded_out.load(Ordering::Acquire);
        self.release("shutdown");
        Exit::Shutdown { faded }
    }

    fn fault(&mut self, why: String) -> Exit {
        error!("the RT callback faulted: {why}");
        self.save();
        self.alarm(AlarmCode::Fault, why.clone());
        self.release("fault");
        Exit::Fault(why)
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
        }
    }

    fn tick(&mut self, now: Instant) -> Option<Exit> {
        self.flush_rt();
        let stats = self.driver.as_ref().map(|d| d.stats()).unwrap_or_default();
        if stats.faulted {
            return Some(self.fault(stats.fault.unwrap_or_else(|| "unknown".into())));
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
            self.broadcast(&EngineMsg::Meters(meters_msg(&frame)));
        }
        if now.saturating_duration_since(self.last_status) >= Duration::from_secs(1) {
            self.last_status = now;
            let status = self.status_msg(&stats);
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
            buses: vec![[1.0, 0.0], [0.125, 2.0]],
            gr_db: vec![-3.0, 0.0],
            active: vec![96_000, 48_000],
            trips: 2,
        };
        let m = meters_msg(&f);
        assert_eq!(m.seq, 3);
        assert_eq!(m.inputs, vec![[0.5f32, 0.25]]);
        assert_eq!(m.buses, vec![[1.0f32, 0.0], [0.125, 2.0]]);
        assert_eq!(m.gr_db, vec![-3.0f32, 0.0]);
        assert_eq!(m.limiter_active_s, vec![1.0, 0.5]);
        assert_eq!(m.trips, 2);
    }

    #[test]
    fn the_build_names_the_version() {
        assert!(engine_build().starts_with(concat!(env!("CARGO_PKG_VERSION"), "+")));
        assert!(unix_ms() > 1_700_000_000_000);
    }
}
