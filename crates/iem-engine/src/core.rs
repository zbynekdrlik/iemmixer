//! The control core (program spec I6, §2.3, X2, X3, X13; S3 design note §3.3;
//! the model of the #20 design note §3):
//! the single writer of the mix state. `apply` is pure — no threads, no I/O —
//! and turns one request into the new revision, the changed entities and one
//! group of RT commands. Every field is capped here, whoever sent it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use iem_engine_proto::{
    Change, Cmd, EqTarget, ErrCode, ErrorBody, GroupId, InputId, InputState, Level, Mix, MixGroup,
    MixId, MixOut, MixState, Solo, Source, TestSignal, Transient, db_to_lin,
};

use crate::cmd::RtOp;
use crate::params::{
    FADER_DB, LIMIT_DB, PAN, Range, TEST_DBFS, TEST_HZ, TEST_TTL_S, TRIM_DB, cap, cap_eq,
    cap_group, cap_input, cap_level, cap_out, eq_is_finite, eq_params, input_params,
};
use crate::topology::Topology;
use crate::{MAX_BATCH, MAX_CMDS_PER_BLOCK, MAX_SOLO, SAMPLE_RATE};

/// Per-run launch flags (§2.3: test signal and fault injection are `dev`-only).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags {
    pub test_signal: bool,
    pub fault_injection: bool,
}

/// What the control loop does besides broadcasting the changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    None,
    SendState,
    SendTopology,
    Save,
    Shutdown,
    /// The whole state was replaced: broadcast `State`; save a baseline too.
    Imported {
        baseline: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub rev: u64,
    pub changes: Vec<Change>,
    pub rt: Vec<RtOp>,
    pub effect: Effect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdError {
    pub code: ErrCode,
    pub msg: String,
}

impl CmdError {
    fn new(code: ErrCode, msg: impl Into<String>) -> Self {
        Self {
            code,
            msg: msg.into(),
        }
    }
}

impl From<CmdError> for ErrorBody {
    fn from(e: CmdError) -> Self {
        Self {
            code: e.code,
            msg: e.msg,
        }
    }
}

/// One mix in topology order: its output, its level slots (every input, then
/// the mixes it hears) and its group strips.
#[derive(Debug, Clone, PartialEq)]
pub struct MixRec {
    pub out: MixOut,
    pub levels: Vec<Level>,
    pub groups: Vec<MixGroup>,
}

/// The mix state in topology order.
#[derive(Debug, Clone, PartialEq)]
pub struct Reconciled {
    pub inputs: Vec<InputState>,
    pub mixes: Vec<MixRec>,
}

/// At most 64 bytes of an id for messages (ids may come from anyone).
fn clip(s: &str) -> &str {
    let mut end = s.len().min(64);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or_default()
}

/// The state for this topology: known ids capped, missing ones defaulted;
/// returns what the topology does not have.
pub fn reconcile(topo: &Topology, state: &MixState) -> (Reconciled, Vec<String>) {
    let inputs = topo
        .inputs
        .iter()
        .map(|n| state.inputs.get(&n.id).map(cap_input).unwrap_or_default())
        .collect();
    let mut dropped: Vec<String> = state
        .inputs
        .keys()
        .filter(|id| topo.input_index(id).is_none())
        .map(|id| format!("input {}", clip(&id.0)))
        .collect();
    let mut mixes = Vec::with_capacity(topo.mixes.len());
    for (m, node) in topo.mixes.iter().enumerate() {
        let given = state.mixes.get(&node.id);
        let mut levels = Vec::with_capacity(topo.levels(m));
        for i in &topo.inputs {
            let level = given.and_then(|x| x.inputs.get(&i.id));
            levels.push(level.map(cap_level).unwrap_or_default());
        }
        for &s in &node.mixes {
            let heard = topo.mixes.get(s).and_then(|h| given?.mixes.get(&h.id));
            levels.push(heard.map(cap_level).unwrap_or_default());
        }
        let groups = topo
            .groups
            .iter()
            .map(|g| {
                given
                    .and_then(|x| x.groups.get(&g.id))
                    .map(cap_group)
                    .unwrap_or_default()
            })
            .collect();
        if let Some(x) = given {
            let mix = clip(&node.id.0);
            dropped.extend(
                x.inputs
                    .keys()
                    .filter(|id| topo.input_index(id).is_none())
                    .map(|id| format!("mix {mix} input {}", clip(&id.0))),
            );
            dropped.extend(
                x.groups
                    .keys()
                    .filter(|id| topo.group_index(id).is_none())
                    .map(|id| format!("mix {mix} group {}", clip(&id.0))),
            );
            dropped.extend(
                x.mixes
                    .keys()
                    .filter(|id| topo.slot(m, &Source::Mix((*id).clone())).is_none())
                    .map(|id| format!("mix {mix} hearing {}", clip(&id.0))),
            );
        }
        mixes.push(MixRec {
            out: given.map(|x| cap_out(&x.out)).unwrap_or_default(),
            levels,
            groups,
        });
    }
    dropped.extend(
        state
            .mixes
            .keys()
            .filter(|id| topo.mix_index(id).is_none())
            .map(|id| format!("mix {}", clip(&id.0))),
    );
    (Reconciled { inputs, mixes }, dropped)
}

/// The protocol form of a reconciled state.
pub fn to_state(topo: &Topology, r: &Reconciled) -> MixState {
    let mixes = topo
        .mixes
        .iter()
        .zip(&r.mixes)
        .enumerate()
        .map(|(m, (node, rec))| {
            let mut mix = Mix {
                out: rec.out,
                ..Mix::default()
            };
            for (k, level) in rec.levels.iter().enumerate() {
                match topo.source(m, k) {
                    Some(Source::Input(id)) => {
                        mix.inputs.insert(id, *level);
                    }
                    Some(Source::Mix(id)) => {
                        mix.mixes.insert(id, *level);
                    }
                    None => {}
                }
            }
            mix.groups = topo
                .groups
                .iter()
                .zip(&rec.groups)
                .map(|(g, s)| (g.id.clone(), *s))
                .collect();
            (node.id.clone(), mix)
        })
        .collect();
    MixState {
        inputs: topo
            .inputs
            .iter()
            .zip(&r.inputs)
            .map(|(n, s)| (n.id.clone(), *s))
            .collect(),
        mixes,
    }
}

/// The end of the load chain (§2.4): defaults with every mix muted.
pub fn defaults_muted(topo: &Topology) -> MixState {
    let mut r = reconcile(topo, &MixState::default()).0;
    for rec in &mut r.mixes {
        rec.out.muted = true;
    }
    to_state(topo, &r)
}

fn ix(i: usize) -> u16 {
    u16::try_from(i).unwrap_or(u16::MAX)
}

fn capped(v: f64, r: Range, what: &str) -> Result<f64, CmdError> {
    cap(v, r).ok_or_else(|| CmdError::new(ErrCode::BadValue, format!("{what} is not finite")))
}

/// Commands allowed inside a batch: state edits only.
fn batchable(cmd: &Cmd) -> bool {
    matches!(
        cmd,
        Cmd::SetInput { .. }
            | Cmd::SetMix { .. }
            | Cmd::SetLevel { .. }
            | Cmd::SetGroup { .. }
            | Cmd::SetEq { .. }
            | Cmd::SetLimiter { .. }
            | Cmd::ResetLimiterStats { .. }
            | Cmd::SetSolo { .. }
            | Cmd::StartListen { .. }
            | Cmd::StopListen { .. }
    )
}

#[derive(Debug, Default)]
struct Partial {
    changes: Vec<Change>,
    rt: Vec<RtOp>,
    effect: Option<Effect>,
    bump: bool,
}

impl Partial {
    fn changed(changes: Vec<Change>, rt: Vec<RtOp>) -> Self {
        Self {
            bump: !changes.is_empty(),
            changes,
            rt,
            effect: None,
        }
    }

    fn effect(effect: Effect) -> Self {
        Self {
            effect: Some(effect),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone)]
pub struct Core {
    topo: Arc<Topology>,
    s: Reconciled,
    /// Per mix: its soloed level slots (X2).
    solo: BTreeMap<usize, BTreeSet<usize>>,
    listen: [Option<usize>; 2],
    test: Option<TestSignal>,
    rev: u64,
    flags: Flags,
}

impl Core {
    pub fn new(topo: Arc<Topology>, state: &MixState, rev: u64, flags: Flags) -> Self {
        let s = reconcile(&topo, state).0;
        Self {
            topo,
            s,
            solo: BTreeMap::new(),
            listen: [None, None],
            test: None,
            rev,
            flags,
        }
    }

    pub fn topology(&self) -> &Arc<Topology> {
        &self.topo
    }

    pub fn rev(&self) -> u64 {
        self.rev
    }

    pub fn flags(&self) -> Flags {
        self.flags
    }

    pub fn state(&self) -> MixState {
        to_state(&self.topo, &self.s)
    }

    pub fn transient(&self) -> Transient {
        let mix_id = |m: &usize| self.topo.mixes.get(*m).map(|n| n.id.clone());
        Transient {
            solo: self
                .solo
                .iter()
                .filter_map(|(m, set)| {
                    Some(Solo {
                        mix: mix_id(m)?,
                        sources: set
                            .iter()
                            .filter_map(|k| self.topo.source(*m, *k))
                            .collect(),
                    })
                })
                .collect(),
            listen: self.listen.map(|l| l.as_ref().and_then(mix_id)),
            test_signal: self.test.clone(),
        }
    }

    pub fn apply(&mut self, cmd: &Cmd) -> Result<Outcome, CmdError> {
        let partial = match cmd {
            Cmd::Batch { ops } => self.batch(ops)?,
            other => self.one(other)?,
        };
        Ok(self.finish(partial))
    }

    /// Clears every solo (X2: 10 s after the controller left).
    pub fn clear_solos(&mut self) -> Outcome {
        let scoped: Vec<usize> = self.solo.keys().copied().collect();
        let mut p = Partial::default();
        for m in scoped {
            self.solo.remove(&m);
            if let Some(n) = self.topo.mixes.get(m) {
                p.changes.push(Change::Solo {
                    mix: n.id.clone(),
                    sources: Vec::new(),
                });
            }
            p.rt.extend(self.level_ops(m));
        }
        p.bump = !p.changes.is_empty();
        self.finish(p)
    }

    /// The test signal's TTL ran out (the RT thread silences it by itself).
    pub fn end_test_signal(&mut self) -> Outcome {
        let p = if self.test.take().is_some() {
            Partial::changed(vec![Change::TestSignal { signal: None }], Vec::new())
        } else {
            Partial::default()
        };
        self.finish(p)
    }

    /// Commands that bring an RT processor to this state.
    pub fn full_sync(&self) -> Vec<RtOp> {
        let mut rt = Vec::new();
        for (i, s) in self.s.inputs.iter().enumerate() {
            rt.push(RtOp::Input {
                i: ix(i),
                p: input_params(s),
            });
            rt.push(RtOp::InputEq {
                i: ix(i),
                eq: eq_params(&s.eq),
            });
        }
        for (m, rec) in self.s.mixes.iter().enumerate() {
            rt.push(out_op(m, &rec.out));
            rt.push(RtOp::MixEq {
                m: ix(m),
                eq: eq_params(&rec.out.eq),
            });
            rt.push(limiter_op(m, &rec.out));
            rt.extend(self.level_ops(m));
            for (g, strip) in rec.groups.iter().enumerate() {
                rt.push(group_op(m, g, strip));
                rt.push(RtOp::GroupEq {
                    m: ix(m),
                    g: ix(g),
                    eq: eq_params(&strip.eq),
                });
            }
        }
        rt
    }

    fn finish(&mut self, p: Partial) -> Outcome {
        if p.bump {
            self.rev += 1;
        }
        Outcome {
            rev: self.rev,
            changes: p.changes,
            rt: p.rt,
            effect: p.effect.unwrap_or(Effect::None),
        }
    }

    fn batch(&mut self, ops: &[Cmd]) -> Result<Partial, CmdError> {
        if ops.len() > MAX_BATCH {
            return Err(CmdError::new(
                ErrCode::BadValue,
                format!("a batch holds at most {MAX_BATCH} commands"),
            ));
        }
        let mut next = self.clone();
        let mut all = Partial::default();
        for op in ops {
            if !batchable(op) {
                return Err(CmdError::new(
                    ErrCode::BadRequest,
                    "a batch holds state edits only",
                ));
            }
            let p = next.one(op)?;
            all.changes.extend(p.changes);
            all.rt.extend(p.rt);
            all.bump |= p.bump;
        }
        if all.rt.len() > MAX_CMDS_PER_BLOCK {
            return Err(CmdError::new(
                ErrCode::BadValue,
                "the batch needs more commands than one block applies",
            ));
        }
        *self = next;
        Ok(all)
    }

    fn input(&self, id: &InputId) -> Result<usize, CmdError> {
        self.topo.input_index(id).ok_or_else(|| {
            CmdError::new(ErrCode::UnknownId, format!("unknown input {}", clip(&id.0)))
        })
    }

    fn mix(&self, id: &MixId) -> Result<usize, CmdError> {
        self.topo.mix_index(id).ok_or_else(|| {
            CmdError::new(ErrCode::UnknownId, format!("unknown mix {}", clip(&id.0)))
        })
    }

    fn group(&self, id: &GroupId) -> Result<usize, CmdError> {
        self.topo.group_index(id).ok_or_else(|| {
            CmdError::new(ErrCode::UnknownId, format!("unknown group {}", clip(&id.0)))
        })
    }

    fn out(&self, m: usize) -> MixOut {
        self.s.mixes.get(m).map(|r| r.out).unwrap_or_default()
    }

    fn level(&self, m: usize, k: usize) -> Level {
        self.s
            .mixes
            .get(m)
            .and_then(|r| r.levels.get(k))
            .copied()
            .unwrap_or_default()
    }

    fn strip(&self, m: usize, g: usize) -> MixGroup {
        self.s
            .mixes
            .get(m)
            .and_then(|r| r.groups.get(g))
            .copied()
            .unwrap_or_default()
    }

    fn one(&mut self, cmd: &Cmd) -> Result<Partial, CmdError> {
        match cmd {
            Cmd::SetInput {
                input,
                trim_db,
                muted,
                processing,
            } => {
                let i = self.input(input)?;
                let mut new = self.s.inputs.get(i).copied().unwrap_or_default();
                if let Some(v) = trim_db {
                    new.trim_db = capped(*v, TRIM_DB, "trim_db")?;
                }
                if let Some(v) = muted {
                    new.muted = *v;
                }
                if let Some(v) = processing {
                    new.processing = *v;
                }
                Ok(self.set_input(i, new))
            }
            Cmd::SetMix {
                mix,
                volume_db,
                muted,
            } => {
                let m = self.mix(mix)?;
                let mut new = self.out(m);
                if let Some(v) = volume_db {
                    new.volume_db = capped(*v, FADER_DB, "volume_db")?;
                }
                if let Some(v) = muted {
                    new.muted = *v;
                }
                Ok(self.set_out(m, new))
            }
            Cmd::SetLevel {
                mix,
                source,
                gain_db,
                pan,
                muted,
            } => {
                let m = self.mix(mix)?;
                let k = self.topo.slot(m, source).ok_or_else(|| {
                    CmdError::new(
                        ErrCode::UnknownId,
                        format!(
                            "mix {} does not hear {}",
                            clip(&mix.0),
                            clip(&source.to_string())
                        ),
                    )
                })?;
                let mut new = self.level(m, k);
                if let Some(v) = gain_db {
                    new.gain_db = capped(*v, FADER_DB, "gain_db")?;
                }
                if let Some(v) = pan {
                    new.pan = capped(*v, PAN, "pan")?;
                }
                if let Some(v) = muted {
                    new.muted = *v;
                }
                Ok(self.set_level(m, k, new))
            }
            Cmd::SetGroup {
                mix,
                group,
                gain_db,
                muted,
            } => {
                let m = self.mix(mix)?;
                let g = self.group(group)?;
                let mut new = self.strip(m, g);
                if let Some(v) = gain_db {
                    new.gain_db = capped(*v, FADER_DB, "gain_db")?;
                }
                if let Some(v) = muted {
                    new.muted = *v;
                }
                Ok(self.set_group(m, g, new))
            }
            Cmd::SetEq { target, eq } => {
                if !eq_is_finite(eq) {
                    return Err(CmdError::new(
                        ErrCode::BadValue,
                        "an EQ value is not finite",
                    ));
                }
                let eq = cap_eq(eq);
                match target {
                    EqTarget::Input(id) => {
                        let i = self.input(id)?;
                        let old = self.s.inputs.get(i).copied().unwrap_or_default();
                        Ok(self.set_input(i, InputState { eq, ..old }))
                    }
                    EqTarget::Mix(id) => {
                        let m = self.mix(id)?;
                        let old = self.out(m);
                        Ok(self.set_out(m, MixOut { eq, ..old }))
                    }
                    EqTarget::Group { mix, group } => {
                        let m = self.mix(mix)?;
                        let g = self.group(group)?;
                        let old = self.strip(m, g);
                        Ok(self.set_group(m, g, MixGroup { eq, ..old }))
                    }
                }
            }
            Cmd::SetLimiter {
                mix,
                enabled,
                limit_db,
            } => {
                let m = self.mix(mix)?;
                let mut new = self.out(m);
                if let Some(v) = limit_db {
                    new.limiter.limit_db = capped(*v, LIMIT_DB, "limit_db")?;
                }
                if let Some(v) = enabled {
                    new.limiter.enabled = *v;
                }
                Ok(self.set_out(m, new))
            }
            Cmd::ResetLimiterStats { mix } => {
                let m = self.mix(mix)?;
                Ok(Partial::changed(
                    vec![Change::LimiterStatsReset { mix: mix.clone() }],
                    vec![RtOp::ResetLimiter { m: ix(m) }],
                ))
            }
            Cmd::SetSolo { mix, sources } => self.set_solo(mix, sources),
            Cmd::StartListen { mix } => self.start_listen(mix),
            Cmd::StopListen { mix } => {
                let m = self.mix(mix)?;
                let mut rt = Vec::new();
                for (slot, l) in self.listen.iter_mut().enumerate() {
                    if *l == Some(m) {
                        *l = None;
                        rt.push(RtOp::Listen {
                            slot: slot as u8,
                            mix: None,
                        });
                    }
                }
                Ok(self.listen_partial(rt))
            }
            Cmd::StartTestSignal {
                input,
                hz,
                dbfs,
                ttl_s,
            } => {
                if !self.flags.test_signal {
                    return Err(CmdError::new(
                        ErrCode::Forbidden,
                        "the engine runs without the test-signal flag",
                    ));
                }
                let i = self.input(input)?;
                let hz = capped(*hz, TEST_HZ, "hz")?;
                let dbfs = capped(*dbfs, TEST_DBFS, "dbfs")?;
                let ttl_s = capped(*ttl_s, TEST_TTL_S, "ttl_s")?;
                let signal = TestSignal {
                    input: input.clone(),
                    hz,
                    dbfs,
                    ttl_s,
                };
                self.test = Some(signal.clone());
                Ok(Partial::changed(
                    vec![Change::TestSignal {
                        signal: Some(signal),
                    }],
                    vec![RtOp::TestSignal {
                        i: ix(i),
                        hz,
                        amp: db_to_lin(dbfs),
                        ttl: (ttl_s * f64::from(SAMPLE_RATE)).round() as u64,
                    }],
                ))
            }
            Cmd::StopTestSignal => Ok(if self.test.take().is_some() {
                Partial::changed(
                    vec![Change::TestSignal { signal: None }],
                    vec![RtOp::StopTestSignal],
                )
            } else {
                Partial::default()
            }),
            Cmd::InjectFault => {
                if !self.flags.fault_injection {
                    return Err(CmdError::new(
                        ErrCode::Forbidden,
                        "the engine runs without the fault-injection flag",
                    ));
                }
                Ok(Partial {
                    rt: vec![RtOp::Panic],
                    ..Partial::default()
                })
            }
            Cmd::Batch { .. } => Err(CmdError::new(ErrCode::BadRequest, "nested batch")),
            Cmd::ImportState { state, baseline } => {
                self.s = reconcile(&self.topo, state).0;
                Ok(Partial {
                    rt: self.full_sync(),
                    effect: Some(Effect::Imported {
                        baseline: *baseline,
                    }),
                    bump: true,
                    ..Partial::default()
                })
            }
            Cmd::GetState => Ok(Partial::effect(Effect::SendState)),
            Cmd::GetTopology => Ok(Partial::effect(Effect::SendTopology)),
            Cmd::SaveNow => Ok(Partial::effect(Effect::Save)),
            Cmd::Shutdown => Ok(Partial::effect(Effect::Shutdown)),
            Cmd::Ping => Ok(Partial::default()),
        }
    }

    fn set_input(&mut self, i: usize, new: InputState) -> Partial {
        let (Some(old), Some(node)) = (self.s.inputs.get_mut(i), self.topo.inputs.get(i)) else {
            return Partial::default();
        };
        if *old == new {
            return Partial::default();
        }
        let mut rt = Vec::new();
        if (InputState { eq: new.eq, ..*old }) != new {
            rt.push(RtOp::Input {
                i: ix(i),
                p: input_params(&new),
            });
        }
        if old.eq != new.eq {
            rt.push(RtOp::InputEq {
                i: ix(i),
                eq: eq_params(&new.eq),
            });
        }
        *old = new;
        Partial::changed(
            vec![Change::Input {
                id: node.id.clone(),
                state: new,
            }],
            rt,
        )
    }

    fn set_out(&mut self, m: usize, new: MixOut) -> Partial {
        let (Some(rec), Some(node)) = (self.s.mixes.get_mut(m), self.topo.mixes.get(m)) else {
            return Partial::default();
        };
        let old = rec.out;
        if old == new {
            return Partial::default();
        }
        let mut rt = Vec::new();
        if (old.volume_db, old.muted) != (new.volume_db, new.muted) {
            rt.push(out_op(m, &new));
        }
        if old.eq != new.eq {
            rt.push(RtOp::MixEq {
                m: ix(m),
                eq: eq_params(&new.eq),
            });
        }
        if old.limiter != new.limiter {
            rt.push(limiter_op(m, &new));
        }
        rec.out = new;
        Partial::changed(
            vec![Change::MixOut {
                mix: node.id.clone(),
                out: new,
            }],
            rt,
        )
    }

    fn set_level(&mut self, m: usize, k: usize, new: Level) -> Partial {
        let (Some(node), Some(source)) = (self.topo.mixes.get(m), self.topo.source(m, k)) else {
            return Partial::default();
        };
        let mix = node.id.clone();
        let Some(slot) = self.s.mixes.get_mut(m).and_then(|r| r.levels.get_mut(k)) else {
            return Partial::default();
        };
        if *slot == new {
            return Partial::default();
        }
        *slot = new;
        Partial::changed(
            vec![Change::Level {
                mix,
                source,
                level: new,
            }],
            vec![self.level_op(m, k)],
        )
    }

    fn set_group(&mut self, m: usize, g: usize, new: MixGroup) -> Partial {
        let (Some(node), Some(group)) = (self.topo.mixes.get(m), self.topo.groups.get(g)) else {
            return Partial::default();
        };
        let (mix, group) = (node.id.clone(), group.id.clone());
        let Some(strip) = self.s.mixes.get_mut(m).and_then(|r| r.groups.get_mut(g)) else {
            return Partial::default();
        };
        let old = *strip;
        if old == new {
            return Partial::default();
        }
        *strip = new;
        let mut rt = Vec::new();
        if (old.gain_db, old.muted) != (new.gain_db, new.muted) {
            rt.push(group_op(m, g, &new));
        }
        if old.eq != new.eq {
            rt.push(RtOp::GroupEq {
                m: ix(m),
                g: ix(g),
                eq: eq_params(&new.eq),
            });
        }
        Partial::changed(
            vec![Change::Group {
                mix,
                group,
                state: new,
            }],
            rt,
        )
    }

    /// A level a solo silences: its mix has a solo that does not hold it.
    fn solo_out(&self, m: usize, k: usize) -> bool {
        self.solo.get(&m).is_some_and(|set| !set.contains(&k))
    }

    fn level_op(&self, m: usize, k: usize) -> RtOp {
        let l = self.level(m, k);
        RtOp::Level {
            m: ix(m),
            k: ix(k),
            gain: db_to_lin(l.gain_db),
            pan: l.pan,
            muted: l.muted || self.solo_out(m, k),
        }
    }

    /// Every level of mix `m`, as the RT thread must hold it.
    fn level_ops(&self, m: usize) -> impl Iterator<Item = RtOp> + '_ {
        (0..self.topo.levels(m)).map(move |k| self.level_op(m, k))
    }

    fn set_solo(&mut self, mix: &MixId, sources: &[Source]) -> Result<Partial, CmdError> {
        let m = self.mix(mix)?;
        if sources.len() > MAX_SOLO {
            return Err(CmdError::new(
                ErrCode::BadValue,
                format!("at most {MAX_SOLO} solo sources"),
            ));
        }
        let mut new = BTreeSet::new();
        for source in sources {
            let k = self.topo.slot(m, source).ok_or_else(|| {
                CmdError::new(
                    ErrCode::BadValue,
                    format!(
                        "{} is not heard in {}",
                        clip(&source.to_string()),
                        clip(&mix.0)
                    ),
                )
            })?;
            new.insert(k);
        }
        if self.solo.get(&m).cloned().unwrap_or_default() == new {
            return Ok(Partial::default());
        }
        let list: Vec<Source> = new.iter().filter_map(|k| self.topo.source(m, *k)).collect();
        if new.is_empty() {
            self.solo.remove(&m);
        } else {
            self.solo.insert(m, new);
        }
        let rt = self.level_ops(m).collect();
        Ok(Partial::changed(
            vec![Change::Solo {
                mix: mix.clone(),
                sources: list,
            }],
            rt,
        ))
    }

    /// X3: slot 0 is the engineer's tap, slot 1 one other mix's.
    fn start_listen(&mut self, mix: &MixId) -> Result<Partial, CmdError> {
        let m = self.mix(mix)?;
        let slot = usize::from(m != self.topo.engineer);
        let current = self.listen.get(slot).copied().flatten();
        if current == Some(m) {
            return Ok(Partial::default());
        }
        if slot == 1 && current.is_some() {
            return Err(CmdError::new(
                ErrCode::NoSource,
                "another mix is being listened to",
            ));
        }
        if let Some(l) = self.listen.get_mut(slot) {
            *l = Some(m);
        }
        Ok(self.listen_partial(vec![RtOp::Listen {
            slot: slot as u8,
            mix: Some(ix(m)),
        }]))
    }

    fn listen_partial(&self, rt: Vec<RtOp>) -> Partial {
        if rt.is_empty() {
            return Partial::default();
        }
        let listen = self.transient().listen;
        Partial::changed(vec![Change::Listen { listen }], rt)
    }
}

fn out_op(m: usize, o: &MixOut) -> RtOp {
    RtOp::MixOut {
        m: ix(m),
        volume: db_to_lin(o.volume_db),
        muted: o.muted,
    }
}

fn limiter_op(m: usize, o: &MixOut) -> RtOp {
    RtOp::Limiter {
        m: ix(m),
        enabled: o.limiter.enabled,
        limit_db: o.limiter.limit_db,
    }
}

fn group_op(m: usize, g: usize, s: &MixGroup) -> RtOp {
    RtOp::Group {
        m: ix(m),
        g: ix(g),
        gain: db_to_lin(s.gain_db),
        muted: s.muted,
    }
}

#[cfg(test)]
#[path = "core_tests.rs"]
mod tests;
