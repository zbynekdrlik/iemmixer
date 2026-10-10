//! The connections (design note §3.3): hello and roles, replies, the
//! broadcasts, and the alarms replayed to each new connection.

use std::io::Write;
use std::sync::Arc;
use std::time::Instant;

use iem_engine_proto::{
    Alarm, AlarmCode, EngineMsg, ErrCode, ErrorBody, Hello, PROTO, Reply, Role, negotiate,
    write_frame,
};
use tracing::{error, info, warn};

use super::Control;
use crate::SAMPLE_RATE;

pub(super) fn engine_build() -> String {
    format!(
        "{}+{}",
        env!("CARGO_PKG_VERSION"),
        option_env!("GITHUB_SHA").unwrap_or("local")
    )
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

pub(super) fn push_alarm(list: &mut Vec<Alarm>, alarm: Alarm) {
    list.push(alarm);
    if list.len() > MAX_ALARMS {
        list.remove(0);
    }
}

impl Control {
    pub(super) fn drop_peer(&mut self, id: u64, why: &str) {
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

    pub(super) fn send(&mut self, id: u64, msg: &EngineMsg) {
        if let Some(bytes) = encode(msg) {
            self.write(id, &bytes);
        }
    }

    /// To every connection that said hello.
    pub(super) fn broadcast(&mut self, msg: &EngineMsg) {
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

    pub(super) fn reply(&mut self, id: u64, request: u64, error: Option<ErrorBody>) {
        let msg = EngineMsg::Reply(Reply {
            id: request,
            rev: self.core.rev(),
            error,
        });
        self.send(id, &msg);
    }

    pub(super) fn alarm(&mut self, code: AlarmCode, detail: String) {
        warn!("alarm {code:?}: {detail}");
        let alarm = Alarm { code, detail };
        push_alarm(&mut self.alarms, alarm.clone());
        self.broadcast(&EngineMsg::Alarm(alarm));
    }

    pub(super) fn state_msg(&self) -> EngineMsg {
        EngineMsg::State {
            rev: self.core.rev(),
            state: self.core.state(),
            transient: self.core.transient(),
        }
    }

    pub(super) fn hello(&mut self, id: u64, proto: u16, role: Role, client: &str) {
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
}
