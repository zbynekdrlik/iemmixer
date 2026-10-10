//! The control core (program spec I6, §2.3, X2, X3, X13; S3 design note §3.3;
//! the model of the #20 design note §3):
//! the single writer of the mix state. `apply` is pure — no threads, no I/O —
//! and turns one request into the new revision, the changed entities and one
//! group of RT commands. Every field is capped here, whoever sent it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use iem_engine_proto::{
    Change, Cmd, ErrCode, ErrorBody, GroupId, InputId, InputState, Level, MixGroup, MixId, MixOut,
    MixState, Solo, Source, TestSignal, Transient, db_to_lin,
};

use crate::cmd::{HilMask, MAX_HIL, RtOp};
use crate::params::{Range, cap, eq_params, input_params};
use crate::topology::Topology;
use crate::{MAX_BATCH, MAX_CMDS_PER_BLOCK, MAX_SOLO};

mod commands;
mod state;

pub use self::state::{MixRec, Reconciled, defaults_muted, reconcile, to_state};

/// The HIL signal's outputs (`HilTestSignal.card_tx`; S6 design note §4,
/// the owner's decision on #9 of 2026-09-28): the HIL slots of the listed
/// card channels among the engine's spare outputs `hil` (the site's
/// `[guard] hil_tx`, `Topology::hil_outputs`), each within the first
/// [`MAX_HIL`]. A mix's TX is refused first, whatever `hil` holds: the HIL
/// signal goes only to spare outputs, never to a channel a band member
/// hears.
pub fn hil_mask(topo: &Topology, hil: &[u16], card_tx: &[u16]) -> Result<HilMask, CmdError> {
    if card_tx.is_empty() {
        return Err(CmdError::new(
            ErrCode::BadValue,
            "the HIL test signal names no card output",
        ));
    }
    let mut mask = [false; MAX_HIL];
    for &ch in card_tx {
        if let Some(why) = topo.mix_tx_refusal(ch) {
            return Err(CmdError::new(ErrCode::Forbidden, why));
        }
        let slot = hil.iter().position(|&c| c == ch).ok_or_else(|| {
            CmdError::new(
                ErrCode::UnknownId,
                format!("card output {ch} is not a HIL output of this engine ([guard] hil_tx)"),
            )
        })?;
        let bit = mask.get_mut(slot).ok_or_else(|| {
            CmdError::new(
                ErrCode::BadValue,
                format!("card output {ch} is beyond the first {MAX_HIL} HIL outputs"),
            )
        })?;
        *bit = true;
    }
    Ok(mask)
}

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
    /// HIL's forced reopen: the backend reopens the card.
    Reopen,
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

/// At most 64 bytes of an id for messages (ids may come from anyone).
fn clip(s: &str) -> &str {
    let mut end = s.len().min(64);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or_default()
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
    /// The running test signal is the HIL one (card-masked): a plain one
    /// may not replace it until it ends.
    hil_test: bool,
    /// HIL's spare card outputs the engine opened after the topology's TX
    /// (S6): the HIL slots of `HilTestSignal`'s mask.
    hil: Vec<u16>,
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
            hil_test: false,
            hil: Vec::new(),
            rev,
            flags,
        }
    }

    /// The core of an engine that opened HIL's spare card outputs `hil`
    /// (S6, checked by `Topology::hil_outputs`) after the topology's TX, in
    /// that order.
    pub fn with_hil(mut self, hil: Vec<u16>) -> Self {
        self.hil = hil;
        self
    }

    /// HIL's spare card outputs (empty unless the engine opened them).
    pub fn hil(&self) -> &[u16] {
        &self.hil
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

    /// Fault injection and HIL's forced reopen run only under the
    /// `--fault-injection` launch flag.
    fn need_fault_injection(&self) -> Result<(), CmdError> {
        if self.flags.fault_injection {
            Ok(())
        } else {
            Err(CmdError::new(
                ErrCode::Forbidden,
                "the engine runs without the fault-injection flag",
            ))
        }
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
mod tests;
