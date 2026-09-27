//! The server's copy of the engine's state (program spec I6; S5 design note
//! §4): the topology, the persisted `MixState`, the transient state (solo,
//! listen, test signal) and the revision. It changes only through the
//! engine's messages; a gap in the revision asks for a resync.

use std::sync::Arc;

use iem_engine_proto::{
    Change, EngineMsg, GroupId, Hello, InputId, InputState, Level, MixGroup, MixId, MixOut,
    MixState, Source, TopologyInfo, Transient,
};

#[derive(Debug, Clone, Default)]
pub struct Mirror {
    pub hello: Option<Hello>,
    pub topology: Option<Arc<TopologyInfo>>,
    pub state: MixState,
    pub transient: Transient,
    pub rev: u64,
    /// A `State` arrived since the last (re)connect.
    pub synced: bool,
}

/// What an engine message did to the mirror.
#[derive(Debug, Clone, PartialEq)]
pub enum MirrorEvent {
    /// A new topology (reconnect or a restarted engine).
    Topology,
    /// The whole state was replaced.
    Reset,
    /// Changes applied at the next revision.
    Changed {
        origin: Option<u64>,
        changes: Vec<Change>,
    },
    /// A delta skipped a revision: the state must be fetched again.
    Resync,
}

impl Mirror {
    /// Forget the state (the connection dropped).
    pub fn disconnected(&mut self) {
        self.synced = false;
    }

    pub fn apply(&mut self, msg: &EngineMsg) -> Option<MirrorEvent> {
        match msg {
            EngineMsg::Hello(h) => {
                self.hello = Some(h.clone());
                None
            }
            EngineMsg::Topology(t) => {
                self.topology = Some(Arc::new(t.clone()));
                Some(MirrorEvent::Topology)
            }
            EngineMsg::State {
                rev,
                state,
                transient,
            } => {
                self.state = state.clone();
                self.transient = transient.clone();
                self.rev = *rev;
                self.synced = true;
                Some(MirrorEvent::Reset)
            }
            EngineMsg::Delta {
                rev,
                origin,
                changes,
            } => {
                if !self.synced || *rev <= self.rev {
                    return None;
                }
                if *rev != self.rev + 1 {
                    self.synced = false;
                    return Some(MirrorEvent::Resync);
                }
                for c in changes {
                    self.change(c);
                }
                self.rev = *rev;
                Some(MirrorEvent::Changed {
                    origin: *origin,
                    changes: changes.clone(),
                })
            }
            _ => None,
        }
    }

    fn change(&mut self, c: &Change) {
        match c {
            Change::Input { id, state } => {
                self.state.inputs.insert(id.clone(), *state);
            }
            Change::MixOut { mix, out } => {
                self.state.mixes.entry(mix.clone()).or_default().out = *out;
            }
            Change::Level { mix, source, level } => {
                let m = self.state.mixes.entry(mix.clone()).or_default();
                match source {
                    Source::Input(id) => {
                        m.inputs.insert(id.clone(), *level);
                    }
                    Source::Mix(id) => {
                        m.mixes.insert(id.clone(), *level);
                    }
                }
            }
            Change::Group { mix, group, state } => {
                self.state
                    .mixes
                    .entry(mix.clone())
                    .or_default()
                    .groups
                    .insert(group.clone(), *state);
            }
            Change::Solo { mix, sources } => {
                self.transient.solo.retain(|s| &s.mix != mix);
                if !sources.is_empty() {
                    self.transient.solo.push(iem_engine_proto::Solo {
                        mix: mix.clone(),
                        sources: sources.clone(),
                    });
                }
            }
            Change::Listen { listen } => self.transient.listen = listen.clone(),
            Change::TestSignal { signal } => self.transient.test_signal = signal.clone(),
            Change::LimiterStatsReset { .. } => {}
        }
    }

    /// The level of `source` in `mix` (the engine's default when absent).
    pub fn level(&self, mix: &MixId, source: &Source) -> Level {
        self.state
            .mixes
            .get(mix)
            .and_then(|m| m.level(source))
            .copied()
            .unwrap_or_default()
    }

    pub fn group(&self, mix: &MixId, group: &GroupId) -> MixGroup {
        self.state
            .mixes
            .get(mix)
            .and_then(|m| m.groups.get(group))
            .copied()
            .unwrap_or_default()
    }

    pub fn out(&self, mix: &MixId) -> MixOut {
        self.state.mixes.get(mix).map(|m| m.out).unwrap_or_default()
    }

    pub fn input(&self, id: &InputId) -> InputState {
        self.state.inputs.get(id).copied().unwrap_or_default()
    }

    /// The soloed sources of `mix` (empty: no solo).
    pub fn solo(&self, mix: &MixId) -> &[Source] {
        self.transient
            .solo
            .iter()
            .find(|s| &s.mix == mix)
            .map_or(&[], |s| s.sources.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iem_engine_proto::{DB_OFF, Mix, Role, Solo};

    fn hello() -> EngineMsg {
        EngineMsg::Hello(Hello {
            proto: 1,
            engine_build: "b".into(),
            topology_hash: "h".into(),
            state_rev: 4,
            sample_rate: 96_000,
            block: 32,
            role: Role::Control,
        })
    }

    fn state(rev: u64) -> EngineMsg {
        let mut s = MixState::default();
        s.mixes.insert(MixId::new("member1"), Mix::default());
        EngineMsg::State {
            rev,
            state: s,
            transient: Transient::default(),
        }
    }

    fn delta(rev: u64, changes: Vec<Change>) -> EngineMsg {
        EngineMsg::Delta {
            rev,
            origin: Some(5),
            changes,
        }
    }

    fn m1() -> MixId {
        MixId::new("member1")
    }

    fn mic() -> Source {
        Source::Input(InputId::new("mic1"))
    }

    #[test]
    fn hello_topology_and_state_sync_the_mirror() {
        let mut m = Mirror::default();
        assert_eq!(m.apply(&hello()), None);
        assert_eq!(m.hello.as_ref().map(|h| h.state_rev), Some(4));
        let topo = TopologyInfo {
            hash: "h".into(),
            sample_rate: 96_000,
            engineer: MixId::new("engineer"),
            inputs: vec![],
            groups: vec![],
            mixes: vec![],
        };
        assert_eq!(
            m.apply(&EngineMsg::Topology(topo.clone())),
            Some(MirrorEvent::Topology)
        );
        assert_eq!(m.topology.as_deref(), Some(&topo));
        assert!(!m.synced);
        assert_eq!(m.apply(&state(4)), Some(MirrorEvent::Reset));
        assert!(m.synced);
        assert_eq!(m.rev, 4);
        assert_eq!(m.apply(&EngineMsg::Superseded), None);
    }

    #[test]
    fn deltas_apply_every_change_kind_at_the_next_revision() {
        let mut m = Mirror::default();
        m.apply(&state(4));
        let level = Level {
            gain_db: -6.0,
            pan: 0.5,
            muted: true,
        };
        let heard = Source::Mix(MixId::new("member2"));
        let input = InputState {
            trim_db: 3.0,
            processing: false,
            muted: true,
            ..InputState::default()
        };
        let out = MixOut {
            volume_db: -3.0,
            muted: true,
            ..MixOut::default()
        };
        let strip = MixGroup {
            gain_db: -9.0,
            muted: true,
            ..MixGroup::default()
        };
        let changes = vec![
            Change::Level {
                mix: m1(),
                source: mic(),
                level,
            },
            Change::Level {
                mix: m1(),
                source: heard.clone(),
                level,
            },
            Change::Input {
                id: InputId::new("mic1"),
                state: input,
            },
            Change::MixOut { mix: m1(), out },
            Change::Group {
                mix: m1(),
                group: GroupId::new("stems"),
                state: strip,
            },
            Change::Solo {
                mix: m1(),
                sources: vec![mic()],
            },
            Change::Listen {
                listen: [Some(MixId::new("engineer")), Some(m1())],
            },
            Change::LimiterStatsReset { mix: m1() },
        ];
        assert_eq!(
            m.apply(&delta(5, changes.clone())),
            Some(MirrorEvent::Changed {
                origin: Some(5),
                changes
            })
        );
        assert_eq!(m.rev, 5);
        assert_eq!(m.level(&m1(), &mic()), level);
        assert_eq!(m.level(&m1(), &heard), level);
        assert_eq!(m.input(&InputId::new("mic1")), input);
        assert_eq!(m.out(&m1()), out);
        assert_eq!(m.group(&m1(), &GroupId::new("stems")), strip);
        assert_eq!(m.solo(&m1()), &[mic()]);
        assert_eq!(m.transient.listen[1], Some(m1()));
        // A solo cleared leaves no entry; another mix's solo stays.
        m.transient.solo.push(Solo {
            mix: MixId::new("member2"),
            sources: vec![mic()],
        });
        m.apply(&delta(
            6,
            vec![Change::Solo {
                mix: m1(),
                sources: vec![],
            }],
        ));
        assert!(m.solo(&m1()).is_empty());
        assert_eq!(m.solo(&MixId::new("member2")), &[mic()]);
    }

    #[test]
    fn old_deltas_are_ignored_and_a_gap_asks_for_a_resync() {
        let mut m = Mirror::default();
        // Before any state nothing applies.
        assert_eq!(m.apply(&delta(1, vec![])), None);
        m.apply(&state(10));
        assert_eq!(m.apply(&delta(10, vec![])), None);
        assert_eq!(m.apply(&delta(9, vec![])), None);
        assert_eq!(m.apply(&delta(12, vec![])), Some(MirrorEvent::Resync));
        assert!(!m.synced);
        assert_eq!(m.rev, 10);
        // Unsynced: further deltas wait for the state.
        assert_eq!(m.apply(&delta(11, vec![])), None);
        m.apply(&state(12));
        assert!(m.apply(&delta(13, vec![])).is_some());
        m.disconnected();
        assert!(!m.synced);
    }

    #[test]
    fn missing_entries_read_as_the_engine_defaults() {
        let m = Mirror::default();
        assert_eq!(m.level(&m1(), &mic()).gain_db, DB_OFF);
        assert_eq!(m.group(&m1(), &GroupId::new("stems")).gain_db, 0.0);
        assert_eq!(m.out(&m1()).volume_db, 0.0);
        assert!(m.input(&InputId::new("x")).processing);
        assert!(m.solo(&m1()).is_empty());
    }
}
