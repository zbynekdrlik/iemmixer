//! The requests (design note §3.3): a frame's checks by role, the core's
//! answer and its effect, and the RT commands it queues.

use std::time::{Duration, Instant};

use iem_engine_proto::{ClientMsg, Cmd, EngineMsg, ErrCode, ErrorBody, Role, parse_client};
use tracing::{info, warn};

use super::Control;
use crate::MAX_CMDS_PER_BLOCK;
use crate::cmd::push_group;
use crate::core::{Effect, Outcome};

impl Control {
    pub(super) fn frame(&mut self, id: u64, bytes: &[u8]) {
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
    pub(super) fn publish(&mut self, out: Outcome, origin: Option<u64>) {
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

    pub(super) fn flush_rt(&mut self) {
        while let Some(group) = self.pending.front() {
            if !push_group(&mut self.cmds, 0, group) {
                return;
            }
            self.pending.pop_front();
        }
    }
}
