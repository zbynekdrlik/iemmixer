//! The mix state against the topology: in topology order with every known
//! id capped and every missing one defaulted, back in the protocol's form,
//! and the load chain's muted defaults.

use iem_engine_proto::{InputState, Level, Mix, MixGroup, MixOut, MixState, Source};

use super::clip;
use crate::params::{cap_group, cap_input, cap_level, cap_out};
use crate::topology::Topology;

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
