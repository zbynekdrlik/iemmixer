//! The mix state against the topology: in topology order with every known
//! id capped and every missing one defaulted, back in the protocol's form,
//! and the load chain's muted defaults.

use iem_engine_proto::shape;
use iem_engine_proto::{InputState, Level, Mix, MixGroup, MixOut, MixState, Source};

use crate::topology::Topology;

/// One mix in topology order: its output, its level slots (every input, then
/// the mixes it hears, then its peers) and its group strips.
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

/// The state for this topology: known ids capped, missing ones defaulted;
/// returns what the topology does not have. The shaping is the protocol's
/// (`iem_engine_proto::shape::reconcile`, which the server uses too, #58
/// re-review); this lays it out in topology order.
pub fn reconcile(topo: &Topology, state: &MixState) -> (Reconciled, Vec<String>) {
    let (shaped, dropped) = shape::reconcile(&topo.info(), state);
    let inputs = topo
        .inputs
        .iter()
        .map(|n| shaped.inputs.get(&n.id).copied().unwrap_or_default())
        .collect();
    let mixes = topo
        .mixes
        .iter()
        .enumerate()
        .map(|(m, node)| {
            let mix = shaped.mixes.get(&node.id);
            let levels = (0..topo.levels(m))
                .map(|k| {
                    let level = match topo.source(m, k) {
                        Some(source) => mix.and_then(|x| x.level(&source)),
                        None => None,
                    };
                    level.copied().unwrap_or_default()
                })
                .collect();
            let groups = topo
                .groups
                .iter()
                .map(|g| {
                    mix.and_then(|x| x.groups.get(&g.id))
                        .copied()
                        .unwrap_or_default()
                })
                .collect();
            MixRec {
                out: mix.map(|x| x.out).unwrap_or_default(),
                levels,
                groups,
            }
        })
        .collect();
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
