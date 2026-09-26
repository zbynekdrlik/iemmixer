//! The control core (program spec I6, §2.3, X2, X3, X13; design note §3.3):
//! the single writer of the mix state. `apply` is pure — no threads, no I/O —
//! and turns one request into the new revision, the changed entities and one
//! group of RT commands. Every field is capped here, whoever sent it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use iem_engine_proto::{
    BusId, BusKind, BusState, Change, Cmd, EqOwner, ErrCode, ErrorBody, InputId, InputState,
    MixState, SendEntry, SendId, SendState, Solo, Source, TestSignal, Transient, db_to_lin,
};

use crate::cmd::RtOp;
use crate::graph::{Graph, Src};
use crate::params::{
    FADER_DB, LIMIT_DB, PAN, Range, TEST_DBFS, TEST_HZ, TEST_TTL_S, TRIM_DB, cap, cap_bus, cap_eq,
    cap_input, cap_send, eq_is_finite, eq_params, input_params,
};
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

/// The mix state in topology order.
#[derive(Debug, Clone, PartialEq)]
pub struct Reconciled {
    pub inputs: Vec<InputState>,
    pub buses: Vec<BusState>,
    pub sends: Vec<SendState>,
}

/// At most 64 bytes of an id for messages (ids may come from anyone).
fn clip(s: &str) -> &str {
    let mut end = s.len().min(64);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or_default()
}

/// The state for this graph: known ids capped, missing ones defaulted;
/// returns the ids the graph does not have.
pub fn reconcile(graph: &Graph, state: &MixState) -> (Reconciled, Vec<String>) {
    let inputs = graph
        .inputs
        .iter()
        .map(|n| state.inputs.get(&n.id).map(cap_input).unwrap_or_default())
        .collect();
    let buses = graph
        .buses
        .iter()
        .map(|n| state.buses.get(&n.id).map(cap_bus).unwrap_or_default())
        .collect();
    let given: HashMap<&SendId, &SendState> =
        state.sends.iter().map(|e| (&e.id, &e.state)).collect();
    let sends = graph
        .sends
        .iter()
        .map(|e| given.get(&e.id).map(|s| cap_send(s)).unwrap_or_default())
        .collect();
    let mut dropped: Vec<String> = state
        .inputs
        .keys()
        .filter(|id| graph.input_index(id).is_none())
        .map(|id| format!("input {}", clip(&id.0)))
        .collect();
    dropped.extend(
        state
            .buses
            .keys()
            .filter(|id| graph.bus_index(id).is_none())
            .map(|id| format!("bus {}", clip(&id.0))),
    );
    dropped.extend(
        state
            .sends
            .iter()
            .filter(|e| graph.send_index(&e.id).is_none())
            .map(|e| format!("send {}", clip(&e.id.to_string()))),
    );
    (
        Reconciled {
            inputs,
            buses,
            sends,
        },
        dropped,
    )
}

/// The protocol form of a reconciled state (sends sorted by id).
pub fn to_mix(graph: &Graph, r: &Reconciled) -> MixState {
    let mut sends: Vec<SendEntry> = graph
        .sends
        .iter()
        .zip(&r.sends)
        .map(|(e, s)| SendEntry {
            id: e.id.clone(),
            state: *s,
        })
        .collect();
    sends.sort_by(|a, b| a.id.cmp(&b.id));
    MixState {
        inputs: graph
            .inputs
            .iter()
            .zip(&r.inputs)
            .map(|(n, s)| (n.id.clone(), *s))
            .collect(),
        buses: graph
            .buses
            .iter()
            .zip(&r.buses)
            .map(|(n, s)| (n.id.clone(), *s))
            .collect(),
        sends,
    }
}

/// The end of the load chain (§2.4): defaults with every TX bus muted.
pub fn defaults_muted(graph: &Graph) -> MixState {
    let mut r = reconcile(graph, &MixState::default()).0;
    for (s, n) in r.buses.iter_mut().zip(&graph.buses) {
        s.muted = n.kind != BusKind::Stems;
    }
    to_mix(graph, &r)
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
            | Cmd::SetBus { .. }
            | Cmd::SetSend { .. }
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
    graph: Arc<Graph>,
    s: Reconciled,
    solo: BTreeMap<usize, BTreeSet<Source>>,
    listen: [Option<usize>; 2],
    test: Option<TestSignal>,
    rev: u64,
    flags: Flags,
    /// The output bus whose tree each bus belongs to (a stems bus: the bus it
    /// feeds; every other bus: itself).
    root: Vec<usize>,
}

impl Core {
    pub fn new(graph: Arc<Graph>, state: &MixState, rev: u64, flags: Flags) -> Self {
        let s = reconcile(&graph, state).0;
        let root = graph
            .buses
            .iter()
            .enumerate()
            .map(|(b, n)| {
                if n.kind != BusKind::Stems {
                    return b;
                }
                graph
                    .sends
                    .iter()
                    .find(|e| e.src == Src::Post(b))
                    .map_or(b, |e| e.dst)
            })
            .collect();
        Self {
            graph,
            s,
            solo: BTreeMap::new(),
            listen: [None, None],
            test: None,
            rev,
            flags,
            root,
        }
    }

    pub fn graph(&self) -> &Arc<Graph> {
        &self.graph
    }

    pub fn rev(&self) -> u64 {
        self.rev
    }

    pub fn flags(&self) -> Flags {
        self.flags
    }

    pub fn state(&self) -> MixState {
        to_mix(&self.graph, &self.s)
    }

    pub fn transient(&self) -> Transient {
        let bus_id = |b: &usize| self.graph.buses.get(*b).map(|n| n.id.clone());
        Transient {
            solo: self
                .solo
                .iter()
                .filter_map(|(scope, set)| {
                    Some(Solo {
                        scope: bus_id(scope)?,
                        sources: set.iter().cloned().collect(),
                    })
                })
                .collect(),
            listen: self.listen.map(|l| l.as_ref().and_then(bus_id)),
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
        let scopes: Vec<usize> = self.solo.keys().copied().collect();
        let mut p = Partial::default();
        for scope in scopes {
            self.solo.remove(&scope);
            if let Some(n) = self.graph.buses.get(scope) {
                p.changes.push(Change::Solo {
                    scope: n.id.clone(),
                    sources: Vec::new(),
                });
            }
            p.rt.extend(self.tree_sends(scope).map(|s| self.send_op(s)));
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
        for (b, s) in self.s.buses.iter().enumerate() {
            rt.push(bus_op(b, s));
            if self.graph.has_eq(b) {
                rt.push(RtOp::BusEq {
                    b: ix(b),
                    eq: eq_params(&s.eq),
                });
            }
            if self.graph.has_limiter(b) {
                rt.push(limiter_op(b, s));
            }
        }
        rt.extend((0..self.s.sends.len()).map(|s| self.send_op(s)));
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
        self.graph.input_index(id).ok_or_else(|| {
            CmdError::new(ErrCode::UnknownId, format!("unknown input {}", clip(&id.0)))
        })
    }

    fn bus(&self, id: &BusId) -> Result<usize, CmdError> {
        self.graph.bus_index(id).ok_or_else(|| {
            CmdError::new(ErrCode::UnknownId, format!("unknown bus {}", clip(&id.0)))
        })
    }

    fn with_limiter(&self, id: &BusId) -> Result<usize, CmdError> {
        let b = self.bus(id)?;
        if self.graph.has_limiter(b) {
            Ok(b)
        } else {
            Err(CmdError::new(
                ErrCode::BadValue,
                format!("bus {} has no limiter", clip(&id.0)),
            ))
        }
    }

    fn one(&mut self, cmd: &Cmd) -> Result<Partial, CmdError> {
        match cmd {
            Cmd::SetInput {
                input,
                trim_db,
                muted,
                processing,
                fader_db,
                pan,
            } => {
                let i = self.input(input)?;
                let mut new = self.s.inputs.get(i).copied().unwrap_or_default();
                if let Some(v) = trim_db {
                    new.trim_db = capped(*v, TRIM_DB, "trim_db")?;
                }
                if let Some(v) = fader_db {
                    new.fader_db = capped(*v, FADER_DB, "fader_db")?;
                }
                if let Some(v) = pan {
                    new.pan = capped(*v, PAN, "pan")?;
                }
                if let Some(v) = muted {
                    new.muted = *v;
                }
                if let Some(v) = processing {
                    new.processing = *v;
                }
                Ok(self.set_input(i, new))
            }
            Cmd::SetBus {
                bus,
                fader_db,
                pan,
                muted,
            } => {
                let b = self.bus(bus)?;
                let mut new = self.s.buses.get(b).copied().unwrap_or_default();
                if let Some(v) = fader_db {
                    new.fader_db = capped(*v, FADER_DB, "fader_db")?;
                }
                if let Some(v) = pan {
                    new.pan = capped(*v, PAN, "pan")?;
                }
                if let Some(v) = muted {
                    new.muted = *v;
                }
                Ok(self.set_bus(b, new))
            }
            Cmd::SetSend {
                id,
                gain_db,
                pan,
                muted,
            } => {
                let s = self.graph.send_index(id).ok_or_else(|| {
                    CmdError::new(
                        ErrCode::UnknownId,
                        format!("unknown send {}", clip(&id.to_string())),
                    )
                })?;
                let mut new = self.s.sends.get(s).copied().unwrap_or_default();
                if let Some(v) = gain_db {
                    new.gain_db = capped(*v, FADER_DB, "gain_db")?;
                }
                if let Some(v) = pan {
                    new.pan = capped(*v, PAN, "pan")?;
                }
                if let Some(v) = muted {
                    new.muted = *v;
                }
                Ok(self.set_send(s, new))
            }
            Cmd::SetEq { owner, eq } => {
                if !eq_is_finite(eq) {
                    return Err(CmdError::new(
                        ErrCode::BadValue,
                        "an EQ value is not finite",
                    ));
                }
                let eq = cap_eq(eq);
                match owner {
                    EqOwner::Input(id) => {
                        let i = self.input(id)?;
                        let old = self.s.inputs.get(i).copied().unwrap_or_default();
                        Ok(self.set_input(i, InputState { eq, ..old }))
                    }
                    EqOwner::Bus(id) => {
                        let b = self.bus(id)?;
                        if !self.graph.has_eq(b) {
                            return Err(CmdError::new(
                                ErrCode::BadValue,
                                format!("bus {} has no EQ", clip(&id.0)),
                            ));
                        }
                        let old = self.s.buses.get(b).copied().unwrap_or_default();
                        Ok(self.set_bus(b, BusState { eq, ..old }))
                    }
                }
            }
            Cmd::SetLimiter {
                bus,
                enabled,
                limit_db,
            } => {
                let b = self.with_limiter(bus)?;
                let mut new = self.s.buses.get(b).copied().unwrap_or_default();
                if let Some(v) = limit_db {
                    new.limiter.limit_db = capped(*v, LIMIT_DB, "limit_db")?;
                }
                if let Some(v) = enabled {
                    new.limiter.enabled = *v;
                }
                Ok(self.set_bus(b, new))
            }
            Cmd::ResetLimiterStats { bus } => {
                let b = self.with_limiter(bus)?;
                Ok(Partial::changed(
                    vec![Change::LimiterStatsReset { bus: bus.clone() }],
                    vec![RtOp::ResetLimiter { b: ix(b) }],
                ))
            }
            Cmd::SetSolo { scope, sources } => self.set_solo(scope, sources),
            Cmd::StartListen { bus } => self.start_listen(bus),
            Cmd::StopListen { bus } => {
                let b = self.bus(bus)?;
                let mut rt = Vec::new();
                for (slot, l) in self.listen.iter_mut().enumerate() {
                    if *l == Some(b) {
                        *l = None;
                        rt.push(RtOp::Listen {
                            slot: slot as u8,
                            bus: None,
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
                self.s = reconcile(&self.graph, state).0;
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
        let (Some(old), Some(node)) = (self.s.inputs.get_mut(i), self.graph.inputs.get(i)) else {
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

    fn set_bus(&mut self, b: usize, new: BusState) -> Partial {
        let (has_eq, has_limiter) = (self.graph.has_eq(b), self.graph.has_limiter(b));
        let (Some(old), Some(node)) = (self.s.buses.get_mut(b), self.graph.buses.get(b)) else {
            return Partial::default();
        };
        if *old == new {
            return Partial::default();
        }
        let mut rt = Vec::new();
        if (old.fader_db, old.pan, old.muted) != (new.fader_db, new.pan, new.muted) {
            rt.push(bus_op(b, &new));
        }
        if has_eq && old.eq != new.eq {
            rt.push(RtOp::BusEq {
                b: ix(b),
                eq: eq_params(&new.eq),
            });
        }
        if has_limiter && old.limiter != new.limiter {
            rt.push(limiter_op(b, &new));
        }
        *old = new;
        Partial::changed(
            vec![Change::Bus {
                id: node.id.clone(),
                state: new,
            }],
            rt,
        )
    }

    fn set_send(&mut self, s: usize, new: SendState) -> Partial {
        let (Some(old), Some(edge)) = (self.s.sends.get_mut(s), self.graph.sends.get(s)) else {
            return Partial::default();
        };
        if *old == new {
            return Partial::default();
        }
        *old = new;
        let id = edge.id.clone();
        Partial::changed(vec![Change::Send { id, state: new }], vec![self.send_op(s)])
    }

    fn in_tree(&self, bus: usize, scope: usize) -> bool {
        self.root.get(bus) == Some(&scope)
    }

    fn tree_sends(&self, scope: usize) -> impl Iterator<Item = usize> + '_ {
        self.graph
            .sends
            .iter()
            .enumerate()
            .filter(move |(_, e)| self.in_tree(e.dst, scope))
            .map(|(s, _)| s)
    }

    /// A send a solo silences: into a soloed tree, from a source that is
    /// neither soloed nor a bus of that tree.
    fn solo_out(&self, s: usize) -> bool {
        let Some(e) = self.graph.sends.get(s) else {
            return false;
        };
        let Some(&scope) = self.root.get(e.dst) else {
            return false;
        };
        let Some(set) = self.solo.get(&scope) else {
            return false;
        };
        match e.src {
            Src::Post(j) if self.in_tree(j, scope) => false,
            _ => !set.contains(&e.id.src),
        }
    }

    fn send_op(&self, s: usize) -> RtOp {
        let st = self.s.sends.get(s).copied().unwrap_or_default();
        RtOp::Send {
            s: ix(s),
            gain: db_to_lin(st.gain_db),
            pan: st.pan,
            muted: st.muted || self.solo_out(s),
        }
    }

    fn set_solo(&mut self, scope: &BusId, sources: &[Source]) -> Result<Partial, CmdError> {
        let b = self.bus(scope)?;
        if self.graph.kind(b) != Some(BusKind::Output) {
            return Err(CmdError::new(
                ErrCode::BadValue,
                format!("{} is not a mix bus", clip(&scope.0)),
            ));
        }
        if sources.len() > MAX_SOLO {
            return Err(CmdError::new(
                ErrCode::BadValue,
                format!("at most {MAX_SOLO} solo sources"),
            ));
        }
        let senders: BTreeSet<&Source> = self
            .tree_sends(b)
            .filter_map(|s| self.graph.sends.get(s))
            .filter(|e| !matches!(e.src, Src::Post(j) if self.in_tree(j, b)))
            .map(|e| &e.id.src)
            .collect();
        if let Some(bad) = sources.iter().find(|s| !senders.contains(s)) {
            return Err(CmdError::new(
                ErrCode::BadValue,
                format!(
                    "{} does not feed {}",
                    clip(&bad.to_string()),
                    clip(&scope.0)
                ),
            ));
        }
        let new: BTreeSet<Source> = sources.iter().cloned().collect();
        if self.solo.get(&b).cloned().unwrap_or_default() == new {
            return Ok(Partial::default());
        }
        let list: Vec<Source> = new.iter().cloned().collect();
        if new.is_empty() {
            self.solo.remove(&b);
        } else {
            self.solo.insert(b, new);
        }
        let rt = self.tree_sends(b).map(|s| self.send_op(s)).collect();
        Ok(Partial::changed(
            vec![Change::Solo {
                scope: scope.clone(),
                sources: list,
            }],
            rt,
        ))
    }

    fn start_listen(&mut self, bus: &BusId) -> Result<Partial, CmdError> {
        let b = self.bus(bus)?;
        let slot = if b == self.graph.engineer {
            0
        } else if self.graph.kind(b) != Some(BusKind::Output) {
            return Err(CmdError::new(
                ErrCode::BadValue,
                format!("{} has no listen tap", clip(&bus.0)),
            ));
        } else {
            1
        };
        let current = self.listen.get(slot).copied().flatten();
        if current == Some(b) {
            return Ok(Partial::default());
        }
        if slot == 1 && current.is_some() {
            return Err(CmdError::new(
                ErrCode::NoSource,
                "another member is being listened to",
            ));
        }
        if let Some(l) = self.listen.get_mut(slot) {
            *l = Some(b);
        }
        Ok(self.listen_partial(vec![RtOp::Listen {
            slot: slot as u8,
            bus: Some(ix(b)),
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

fn bus_op(b: usize, s: &BusState) -> RtOp {
    RtOp::Bus {
        b: ix(b),
        fader: db_to_lin(s.fader_db),
        pan: s.pan,
        muted: s.muted,
    }
}

fn limiter_op(b: usize, s: &BusState) -> RtOp {
    RtOp::Limiter {
        b: ix(b),
        enabled: s.limiter.enabled,
        limit_db: s.limiter.limit_db,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_site;
    use iem_engine_proto::{Eq, Limiter};

    fn core(flags: Flags) -> Core {
        Core::new(Arc::new(test_site()), &MixState::default(), 0, flags)
    }

    fn bus(s: &str) -> BusId {
        BusId::new(s)
    }

    fn input(s: &str) -> InputId {
        InputId::new(s)
    }

    fn set_bus(name: &str, fader_db: Option<f64>, pan: Option<f64>) -> Cmd {
        Cmd::SetBus {
            bus: bus(name),
            fader_db,
            pan,
            muted: None,
        }
    }

    fn send(src: Source, dst: &str) -> SendId {
        SendId { src, dst: bus(dst) }
    }

    fn set_send_gain(src: Source, dst: &str, gain_db: f64) -> Cmd {
        Cmd::SetSend {
            id: send(src, dst),
            gain_db: Some(gain_db),
            pan: None,
            muted: None,
        }
    }

    fn code(r: Result<Outcome, CmdError>) -> ErrCode {
        r.unwrap_err().code
    }

    #[test]
    fn a_set_changes_state_bumps_rev_and_emits_one_rt_op() {
        let mut c = core(Flags::default());
        let out = c.apply(&set_bus("member1", Some(-3.0), None)).unwrap();
        assert_eq!(out.rev, 1);
        assert_eq!(out.effect, Effect::None);
        let b = c.graph().bus_index(&bus("member1")).unwrap();
        let state = BusState {
            fader_db: -3.0,
            ..BusState::default()
        };
        assert_eq!(
            out.changes,
            vec![Change::Bus {
                id: bus("member1"),
                state
            }]
        );
        assert_eq!(
            out.rt,
            vec![RtOp::Bus {
                b: b as u16,
                fader: 10f64.powf(-3.0 / 20.0),
                pan: 0.0,
                muted: false
            }]
        );
        assert_eq!(c.state().buses[&bus("member1")], state);
        assert_eq!(c.rev(), 1);
    }

    #[test]
    fn an_unchanged_set_keeps_rev() {
        let mut c = core(Flags::default());
        c.apply(&set_bus("member1", Some(-3.0), None)).unwrap();
        let out = c.apply(&set_bus("member1", Some(-3.0), None)).unwrap();
        assert_eq!((out.rev, out.changes.len(), out.rt.len()), (1, 0, 0));
        let none = c
            .apply(&Cmd::SetInput {
                input: input("mic1"),
                trim_db: None,
                muted: None,
                processing: None,
                fader_db: None,
                pan: None,
            })
            .unwrap();
        assert_eq!((none.rev, none.changes.len()), (1, 0));
    }

    #[test]
    fn values_are_capped_and_non_finite_rejected() {
        let mut c = core(Flags::default());
        c.apply(&set_bus("member2", Some(40.0), Some(3.0))).unwrap();
        let s = c.state().buses[&bus("member2")];
        assert_eq!((s.fader_db, s.pan), (12.0, 1.0));
        let before = c.state();
        assert_eq!(
            code(c.apply(&set_bus("member2", Some(f64::NAN), None))),
            ErrCode::BadValue
        );
        assert_eq!(
            code(c.apply(&set_bus("member2", None, Some(f64::INFINITY)))),
            ErrCode::BadValue
        );
        assert_eq!(c.state(), before);
        assert_eq!(c.rev(), 1);
        c.apply(&Cmd::SetInput {
            input: input("mic1"),
            trim_db: Some(1e308),
            muted: Some(true),
            processing: Some(false),
            fader_db: Some(-1e308),
            pan: Some(-2.0),
        })
        .unwrap();
        let i = c.state().inputs[&input("mic1")];
        assert_eq!(
            (i.trim_db, i.fader_db, i.pan, i.muted, i.processing),
            (24.0, -150.0, -1.0, true, false)
        );
        c.apply(&set_send_gain(
            Source::Input(input("mic1")),
            "member1",
            99.0,
        ))
        .unwrap();
        let st = c.state();
        let e = st
            .sends
            .iter()
            .find(|e| e.id == send(Source::Input(input("mic1")), "member1"))
            .unwrap();
        assert_eq!(e.state.gain_db, 12.0);
        assert_eq!(
            code(c.apply(&Cmd::SetSend {
                id: send(Source::Input(input("mic1")), "member1"),
                gain_db: None,
                pan: Some(f64::NAN),
                muted: None
            })),
            ErrCode::BadValue
        );
        let mut eq = Eq::default();
        eq.bands[2].freq_hz = f64::NAN;
        assert_eq!(
            code(c.apply(&Cmd::SetEq {
                owner: EqOwner::Input(input("mic1")),
                eq
            })),
            ErrCode::BadValue
        );
        eq.bands[2].freq_hz = 1e7;
        eq.bands[2].enabled = true;
        let out = c
            .apply(&Cmd::SetEq {
                owner: EqOwner::Input(input("mic1")),
                eq,
            })
            .unwrap();
        assert_eq!(
            c.state().inputs[&input("mic1")].eq.bands[2].freq_hz,
            24_000.0
        );
        assert!(matches!(out.rt.as_slice(), [RtOp::InputEq { i: 0, .. }]));
        assert_eq!(
            code(c.apply(&Cmd::SetLimiter {
                bus: bus("member1"),
                enabled: None,
                limit_db: Some(f64::NAN)
            })),
            ErrCode::BadValue
        );
    }

    #[test]
    fn unknown_ids_are_unknown_id_with_short_messages() {
        let mut c = core(Flags::default());
        let long = "x".repeat(10_000);
        let cases = vec![
            set_bus("nope", Some(0.0), None),
            set_bus(&long, Some(0.0), None),
            Cmd::SetInput {
                input: input(&long),
                trim_db: None,
                muted: None,
                processing: None,
                fader_db: None,
                pan: None,
            },
            set_send_gain(Source::Input(input("mic1")), "member1.stems", 0.0),
            set_send_gain(Source::Bus(bus(&long)), "member1", 0.0),
            Cmd::SetEq {
                owner: EqOwner::Bus(bus("nope")),
                eq: Eq::default(),
            },
            Cmd::StartListen { bus: bus("nope") },
            Cmd::SetSolo {
                scope: bus("nope"),
                sources: vec![],
            },
        ];
        for cmd in cases {
            let e = c.apply(&cmd).unwrap_err();
            assert_eq!(e.code, ErrCode::UnknownId, "{cmd:?}");
            assert!(e.msg.len() < 100, "{}", e.msg.len());
        }
        assert_eq!(clip("é".repeat(40).as_str()).len(), 64);
        // Byte 64 inside a character: cut before it (in a thread, so that a
        // cut that never finds a boundary fails instead of hanging).
        let odd = format!("{}é", "a".repeat(63));
        let (tx, rx) = std::sync::mpsc::channel();
        let text = odd.clone();
        let _ = std::thread::spawn(move || tx.send(clip(&text).to_owned()));
        let cut = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("clip returns");
        assert_eq!(cut, odd[..63]);
        assert_eq!(c.rev(), 0);
    }

    #[test]
    fn batches_are_atomic_and_bounded() {
        let mut c = core(Flags::default());
        let good = Cmd::Batch {
            ops: vec![
                set_bus("member1", Some(-1.0), None),
                set_bus("member2", Some(-2.0), None),
            ],
        };
        let out = c.apply(&good).unwrap();
        assert_eq!((out.rev, out.changes.len(), out.rt.len()), (1, 2, 2));
        let failing = Cmd::Batch {
            ops: vec![
                set_bus("member3", Some(-3.0), None),
                set_bus("member4", Some(-4.0), None),
                set_bus("member5", Some(f64::NAN), None),
            ],
        };
        assert_eq!(code(c.apply(&failing)), ErrCode::BadValue);
        assert_eq!(c.state().buses[&bus("member3")].fader_db, 0.0);
        assert_eq!(c.rev(), 1);
        let big = Cmd::Batch {
            ops: vec![Cmd::Ping; MAX_BATCH + 1],
        };
        assert_eq!(code(c.apply(&big)), ErrCode::BadValue);
        for inner in [
            Cmd::Batch { ops: vec![] },
            Cmd::Shutdown,
            Cmd::SaveNow,
            Cmd::GetState,
            Cmd::InjectFault,
            Cmd::ImportState {
                state: MixState::default(),
                baseline: false,
            },
            Cmd::StartTestSignal {
                input: input("mic1"),
                hz: 1000.0,
                dbfs: -30.0,
                ttl_s: 1.0,
            },
        ] {
            let nested = Cmd::Batch { ops: vec![inner] };
            assert_eq!(code(c.apply(&nested)), ErrCode::BadRequest);
        }
        let empty = c.apply(&Cmd::Batch { ops: vec![] }).unwrap();
        assert_eq!((empty.rev, empty.changes.len()), (1, 0));
        // 256 solo toggles on the engineer tree need more than 512 RT commands.
        let flip = |on: bool| Cmd::SetSolo {
            scope: bus("engineer"),
            sources: if on {
                vec![Source::Input(input("mic1"))]
            } else {
                vec![]
            },
        };
        let solos = Cmd::Batch {
            ops: (0..MAX_BATCH).map(|k| flip(k % 2 == 0)).collect(),
        };
        assert_eq!(code(c.apply(&solos)), ErrCode::BadValue);
        assert!(c.transient().solo.is_empty());
        // Exactly 256 commands are one batch.
        let full = Cmd::Batch {
            ops: (1..=MAX_BATCH)
                .map(|k| set_bus("member4", Some(-0.1 * k as f64), None))
                .collect(),
        };
        let out = c.apply(&full).unwrap();
        assert_eq!(
            (out.rev, out.changes.len(), out.rt.len()),
            (2, MAX_BATCH, MAX_BATCH)
        );
        // Exactly 512 RT commands fit one block: 20 solo toggles on member3
        // (25 sends each) and 12 fader moves.
        let member3 = |on: bool| Cmd::SetSolo {
            scope: bus("member3"),
            sources: if on {
                vec![Source::Input(input("mic2"))]
            } else {
                vec![]
            },
        };
        let mut ops: Vec<Cmd> = (0..20).map(|k| member3(k % 2 == 0)).collect();
        ops.extend((1..=12).map(|k| set_bus("member6", Some(-f64::from(k)), None)));
        let out = c.apply(&Cmd::Batch { ops }).unwrap();
        assert_eq!((out.rev, out.rt.len()), (3, MAX_CMDS_PER_BLOCK));
        assert!(c.transient().solo.is_empty());
    }

    #[test]
    fn solo_mutes_the_tree_and_clears() {
        let mut c = core(Flags::default());
        let g = Arc::clone(c.graph());
        let scope = g.bus_index(&bus("member3")).unwrap();
        let stems = g.bus_index(&bus("member3.stems")).unwrap();
        let out = c
            .apply(&Cmd::SetSolo {
                scope: bus("member3"),
                sources: vec![Source::Input(input("mic2"))],
            })
            .unwrap();
        assert_eq!(out.rev, 1);
        assert_eq!(
            out.changes,
            vec![Change::Solo {
                scope: bus("member3"),
                sources: vec![Source::Input(input("mic2"))]
            }]
        );
        // 17 direct sends and the stems return into member3, 7 into its stems bus.
        assert_eq!(out.rt.len(), 25);
        for op in &out.rt {
            let RtOp::Send { s, muted, .. } = *op else {
                panic!("{op:?}")
            };
            let e = &g.sends[usize::from(s)];
            assert!(e.dst == scope || e.dst == stems);
            let open = e.id.src == Source::Input(input("mic2")) || e.src == Src::Post(stems);
            assert_eq!(muted, !open, "{}", e.id);
        }
        assert_eq!(c.transient().solo.len(), 1);
        // The stored send state is untouched.
        assert!(c.state().sends.iter().all(|e| !e.state.muted));
        // A send outside the tree is not affected by the solo.
        let other = c
            .apply(&set_send_gain(
                Source::Input(input("mic1")),
                "member4",
                -6.0,
            ))
            .unwrap();
        assert!(matches!(
            other.rt.as_slice(),
            [RtOp::Send { muted: false, .. }]
        ));
        // Soloing a stems-group source keeps its stems return open.
        c.apply(&Cmd::SetSolo {
            scope: bus("member3"),
            sources: vec![Source::Input(input("drums"))],
        })
        .unwrap();
        let same = c
            .apply(&Cmd::SetSolo {
                scope: bus("member3"),
                sources: vec![Source::Input(input("drums"))],
            })
            .unwrap();
        assert_eq!(same.changes.len(), 0);
        let cleared = c.clear_solos();
        assert_eq!(cleared.rev, 4);
        assert_eq!(
            cleared.changes,
            vec![Change::Solo {
                scope: bus("member3"),
                sources: vec![]
            }]
        );
        assert!(
            cleared
                .rt
                .iter()
                .all(|op| matches!(op, RtOp::Send { muted: false, .. }))
        );
        assert_eq!(cleared.rt.len(), 25);
        assert!(c.transient().solo.is_empty());
        assert_eq!(c.clear_solos().rev, 4);
        // Bus-to-bus: soloing member2 on the elevated member silences the others.
        let out = c
            .apply(&Cmd::SetSolo {
                scope: bus("member1"),
                sources: vec![Source::Bus(bus("member2"))],
            })
            .unwrap();
        let m1 = g.bus_index(&bus("member1")).unwrap();
        for op in &out.rt {
            let RtOp::Send { s, muted, .. } = *op else {
                panic!("{op:?}")
            };
            let e = &g.sends[usize::from(s)];
            if e.dst == m1 && matches!(e.src, Src::Post(_)) {
                let open = e.id.src == Source::Bus(bus("member2"))
                    || e.id.src == Source::Bus(bus("member1.stems"));
                assert_eq!(muted, !open, "{}", e.id);
            }
        }
        let off = c
            .apply(&Cmd::SetSolo {
                scope: bus("member1"),
                sources: vec![],
            })
            .unwrap();
        assert_eq!(
            off.changes,
            vec![Change::Solo {
                scope: bus("member1"),
                sources: vec![]
            }]
        );
    }

    #[test]
    fn solo_rejects_foreign_sources_and_non_mix_scopes() {
        let mut c = core(Flags::default());
        for (scope, source) in [
            ("member3", Source::Input(input("drums2"))),
            ("member3", Source::Bus(bus("member2"))),
            ("member3", Source::Bus(bus("member3.stems"))),
            ("member1", Source::Input(input("hand1x"))),
        ] {
            let e = c
                .apply(&Cmd::SetSolo {
                    scope: bus(scope),
                    sources: vec![source],
                })
                .unwrap_err();
            assert_eq!(e.code, ErrCode::BadValue);
        }
        for scope in ["member3.stems", "translator", "master"] {
            let e = c
                .apply(&Cmd::SetSolo {
                    scope: bus(scope),
                    sources: vec![],
                })
                .unwrap_err();
            assert_eq!(e.code, ErrCode::BadValue, "{scope}");
        }
        let many = Cmd::SetSolo {
            scope: bus("member3"),
            sources: vec![Source::Input(input("mic1")); MAX_SOLO + 1],
        };
        assert_eq!(code(c.apply(&many)), ErrCode::BadValue);
        let dup = Cmd::SetSolo {
            scope: bus("member3"),
            sources: vec![Source::Input(input("mic1")); MAX_SOLO],
        };
        assert_eq!(c.apply(&dup).unwrap().changes.len(), 1);
    }

    #[test]
    fn listen_allows_the_engineer_and_one_member() {
        let mut c = core(Flags::default());
        let g = Arc::clone(c.graph());
        let eng = g.engineer as u16;
        let m4 = g.bus_index(&bus("member4")).unwrap() as u16;
        let out = c
            .apply(&Cmd::StartListen {
                bus: bus("engineer"),
            })
            .unwrap();
        assert_eq!(
            out.rt,
            vec![RtOp::Listen {
                slot: 0,
                bus: Some(eng)
            }]
        );
        assert_eq!(
            out.changes,
            vec![Change::Listen {
                listen: [Some(bus("engineer")), None]
            }]
        );
        let out = c
            .apply(&Cmd::StartListen {
                bus: bus("member4"),
            })
            .unwrap();
        assert_eq!(
            out.rt,
            vec![RtOp::Listen {
                slot: 1,
                bus: Some(m4)
            }]
        );
        assert_eq!(
            c.apply(&Cmd::StartListen {
                bus: bus("member4")
            })
            .unwrap()
            .changes
            .len(),
            0
        );
        let e = c
            .apply(&Cmd::StartListen {
                bus: bus("member5"),
            })
            .unwrap_err();
        assert_eq!(e.code, ErrCode::NoSource);
        for no_tap in ["member4.stems", "translator", "master"] {
            assert_eq!(
                code(c.apply(&Cmd::StartListen { bus: bus(no_tap) })),
                ErrCode::BadValue
            );
        }
        let out = c
            .apply(&Cmd::StopListen {
                bus: bus("member4"),
            })
            .unwrap();
        assert_eq!(out.rt, vec![RtOp::Listen { slot: 1, bus: None }]);
        assert_eq!(c.transient().listen, [Some(bus("engineer")), None]);
        assert_eq!(
            c.apply(&Cmd::StopListen {
                bus: bus("member4")
            })
            .unwrap()
            .rt
            .len(),
            0
        );
        c.apply(&Cmd::StartListen {
            bus: bus("member5"),
        })
        .unwrap();
        c.apply(&Cmd::StopListen {
            bus: bus("engineer"),
        })
        .unwrap();
        assert_eq!(c.transient().listen, [None, Some(bus("member5"))]);
    }

    #[test]
    fn test_signal_needs_the_flag_and_is_capped() {
        let start = |dbfs: f64, ttl_s: f64| Cmd::StartTestSignal {
            input: input("mic3"),
            hz: 5.0,
            dbfs,
            ttl_s,
        };
        let mut off = core(Flags::default());
        assert_eq!(code(off.apply(&start(-30.0, 1.0))), ErrCode::Forbidden);
        let mut c = core(Flags {
            test_signal: true,
            fault_injection: false,
        });
        let out = c.apply(&start(-3.0, 500.0)).unwrap();
        assert_eq!(out.rev, 1);
        let RtOp::TestSignal { i, hz, amp, ttl } = out.rt[0] else {
            panic!("{:?}", out.rt)
        };
        assert_eq!((i, hz, ttl), (2, 20.0, 120 * 96_000));
        assert!((amp - 0.1).abs() < 1e-15, "{amp}");
        let t = c.transient().test_signal.unwrap();
        assert_eq!((t.dbfs, t.ttl_s, t.hz), (-20.0, 120.0, 20.0));
        let out = c.apply(&start(-60.0, 0.5)).unwrap();
        assert!(matches!(out.rt[0], RtOp::TestSignal { ttl: 48_000, .. }));
        assert_eq!(code(c.apply(&start(f64::NAN, 1.0))), ErrCode::BadValue);
        let stop = c.apply(&Cmd::StopTestSignal).unwrap();
        assert_eq!(stop.rt, vec![RtOp::StopTestSignal]);
        assert_eq!(stop.changes, vec![Change::TestSignal { signal: None }]);
        assert_eq!(c.apply(&Cmd::StopTestSignal).unwrap().changes.len(), 0);
        c.apply(&start(-30.0, 1.0)).unwrap();
        let ended = c.end_test_signal();
        assert_eq!(ended.changes, vec![Change::TestSignal { signal: None }]);
        assert!(ended.rt.is_empty());
        assert_eq!(c.end_test_signal().changes.len(), 0);
        assert!(c.transient().test_signal.is_none());
    }

    #[test]
    fn fault_injection_needs_the_flag() {
        let mut off = core(Flags::default());
        assert_eq!(code(off.apply(&Cmd::InjectFault)), ErrCode::Forbidden);
        let mut on = core(Flags {
            test_signal: false,
            fault_injection: true,
        });
        let out = on.apply(&Cmd::InjectFault).unwrap();
        assert_eq!((out.rev, out.rt), (0, vec![RtOp::Panic]));
        assert!(on.flags().fault_injection);
    }

    #[test]
    fn import_replaces_state_and_fits_one_block() {
        let mut c = core(Flags::default());
        c.apply(&set_bus("member1", Some(-9.0), None)).unwrap();
        let mut state = MixState::default();
        state.buses.insert(
            bus("member2"),
            BusState {
                fader_db: 50.0,
                ..BusState::default()
            },
        );
        state.buses.insert(bus("ghost"), BusState::default());
        state.inputs.insert(input("ghost"), InputState::default());
        state.sends.push(SendEntry {
            id: send(Source::Input(input("ghost")), "member1"),
            state: SendState::default(),
        });
        let (r, dropped) = reconcile(c.graph(), &state);
        assert_eq!(
            dropped,
            vec!["input ghost", "bus ghost", "send ghost>member1"]
        );
        assert_eq!(r.sends.len(), 268);
        let out = c
            .apply(&Cmd::ImportState {
                state,
                baseline: true,
            })
            .unwrap();
        assert_eq!(out.rev, 2);
        assert_eq!(out.effect, Effect::Imported { baseline: true });
        assert!(out.rt.len() <= MAX_CMDS_PER_BLOCK);
        assert_eq!(out.rt.len(), 24 * 2 + 22 + 20 + 10 + 268);
        let st = c.state();
        assert_eq!(st.buses[&bus("member2")].fader_db, 12.0);
        assert_eq!(st.buses[&bus("member1")].fader_db, 0.0);
        assert!(!st.buses.contains_key(&bus("ghost")));
        assert_eq!(st.sends.len(), 268);
        assert!(st.sends.windows(2).all(|w| w[0].id < w[1].id));
        // A core rebuilt from the state carries the same state and revision.
        let again = Core::new(Arc::clone(c.graph()), &st, c.rev(), Flags::default());
        assert_eq!(again.state(), st);
        assert_eq!(again.rev(), 2);
        assert_eq!(again.full_sync(), c.full_sync());
    }

    #[test]
    fn eq_and_limiter_only_where_the_bus_has_them() {
        let mut c = core(Flags::default());
        for b in ["translator", "master"] {
            assert_eq!(
                code(c.apply(&Cmd::SetEq {
                    owner: EqOwner::Bus(bus(b)),
                    eq: Eq::default()
                })),
                ErrCode::BadValue
            );
        }
        for b in ["member1.stems", "translator", "master"] {
            assert_eq!(
                code(c.apply(&Cmd::SetLimiter {
                    bus: bus(b),
                    enabled: Some(false),
                    limit_db: None
                })),
                ErrCode::BadValue
            );
            assert_eq!(
                code(c.apply(&Cmd::ResetLimiterStats { bus: bus(b) })),
                ErrCode::BadValue
            );
        }
        let mut eq = Eq::default();
        eq.bands[0].enabled = true;
        let out = c
            .apply(&Cmd::SetEq {
                owner: EqOwner::Bus(bus("member1.stems")),
                eq,
            })
            .unwrap();
        assert!(matches!(out.rt.as_slice(), [RtOp::BusEq { .. }]));
        let out = c
            .apply(&Cmd::SetLimiter {
                bus: bus("engineer"),
                enabled: Some(false),
                limit_db: Some(-9.0),
            })
            .unwrap();
        let b = c.graph().engineer as u16;
        assert_eq!(
            out.rt,
            vec![RtOp::Limiter {
                b,
                enabled: false,
                limit_db: -6.0
            }]
        );
        assert_eq!(
            c.state().buses[&bus("engineer")].limiter,
            Limiter {
                enabled: false,
                limit_db: -6.0
            }
        );
        let reset = c
            .apply(&Cmd::ResetLimiterStats {
                bus: bus("engineer"),
            })
            .unwrap();
        assert_eq!(reset.rt, vec![RtOp::ResetLimiter { b }]);
        assert_eq!(
            reset.changes,
            vec![Change::LimiterStatsReset {
                bus: bus("engineer")
            }]
        );
        assert_eq!(reset.rev, 3);
        // A pan change on a bus with EQ and limiter emits only the bus op.
        let pan = c.apply(&set_bus("engineer", None, Some(0.5))).unwrap();
        assert!(matches!(pan.rt.as_slice(), [RtOp::Bus { pan, .. }] if *pan == 0.5));
    }

    #[test]
    fn read_only_commands_and_effects() {
        let mut c = core(Flags::default());
        let cases = [
            (Cmd::GetState, Effect::SendState),
            (Cmd::GetTopology, Effect::SendTopology),
            (Cmd::SaveNow, Effect::Save),
            (Cmd::Shutdown, Effect::Shutdown),
            (Cmd::Ping, Effect::None),
        ];
        for (cmd, effect) in cases {
            let out = c.apply(&cmd).unwrap();
            assert_eq!(
                (out.rev, out.effect, out.changes.len(), out.rt.len()),
                (0, effect, 0, 0)
            );
        }
    }

    #[test]
    fn defaults_mute_every_tx_bus_only() {
        let g = test_site();
        let d = defaults_muted(&g);
        for n in &g.buses {
            assert_eq!(d.buses[&n.id].muted, n.kind != BusKind::Stems, "{}", n.id);
        }
        assert_eq!(d.inputs.len(), 24);
        assert!(d.sends.iter().all(|e| e.state == SendState::default()));
    }

    #[test]
    fn full_sync_covers_every_node() {
        let c = core(Flags::default());
        let ops = c.full_sync();
        let count = |f: fn(&RtOp) -> bool| ops.iter().filter(|o| f(o)).count();
        assert_eq!(count(|o| matches!(o, RtOp::Input { .. })), 24);
        assert_eq!(count(|o| matches!(o, RtOp::InputEq { .. })), 24);
        assert_eq!(count(|o| matches!(o, RtOp::Bus { .. })), 22);
        assert_eq!(count(|o| matches!(o, RtOp::BusEq { .. })), 20);
        assert_eq!(count(|o| matches!(o, RtOp::Limiter { .. })), 10);
        assert_eq!(
            count(|o| matches!(o, RtOp::Send { gain, .. } if *gain == 0.0)),
            268
        );
        let err: ErrorBody = CmdError::new(ErrCode::Forbidden, "x").into();
        assert_eq!(err.code, ErrCode::Forbidden);
    }
}
