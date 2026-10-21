//! A mix state shaped by a topology, exactly as the engine holds it (#58
//! re-review; moved here from the engine's `core::state` and `params` so the
//! server shapes the state it imports the same way): every value capped to
//! its range, every slot the topology has present (missing ones defaulted),
//! what it lacks dropped and named. The engine caps every field from any
//! sender (§2.3: no pipe tokens, so the engine trusts nothing).

use std::collections::BTreeMap;

use crate::ids::{GroupId, InputId, MixId};
use crate::msg::TopologyInfo;
use crate::state::{DB_OFF, Eq, EqBand, InputState, Level, Mix, MixGroup, MixOut, MixState};

/// Inclusive ranges.
pub type Range = (f64, f64);
pub const TRIM_DB: Range = (DB_OFF, 24.0);
/// F5/F7: levels, group faders and mix volumes up to +12 dB; at or below
/// `DB_OFF` silent.
pub const FADER_DB: Range = (DB_OFF, 12.0);
pub const PAN: Range = (-1.0, 1.0);
/// ReaEQ's range (A12).
pub const EQ_FREQ: Range = (20.0, 24_000.0);
/// Up to ReaEQ's +12.04 dB (linear 4).
pub const EQ_GAIN_DB: Range = (DB_OFF, 12.041199826559248);
pub const EQ_BW: Range = (0.01, 4.0);
/// F12.
pub const LIMIT_DB: Range = (-6.0, 0.0);

/// `v` clamped into `r`; `None` when `v` is not finite.
pub fn cap(v: f64, r: Range) -> Option<f64> {
    v.is_finite().then(|| v.clamp(r.0, r.1))
}

fn fix(v: f64, r: Range, default: f64) -> f64 {
    cap(v, r).unwrap_or(default)
}

/// Every EQ value finite (commands reject the rest; loads replace it).
pub fn eq_is_finite(e: &Eq) -> bool {
    e.gain_db.is_finite()
        && e.bands
            .iter()
            .all(|b| b.freq_hz.is_finite() && b.gain_db.is_finite() && b.bw_oct.is_finite())
}

pub fn cap_eq(e: &Eq) -> Eq {
    let d = EqBand::default();
    Eq {
        gain_db: fix(e.gain_db, EQ_GAIN_DB, 0.0),
        bands: e.bands.map(|b| EqBand {
            freq_hz: fix(b.freq_hz, EQ_FREQ, d.freq_hz),
            gain_db: fix(b.gain_db, EQ_GAIN_DB, d.gain_db),
            bw_oct: fix(b.bw_oct, EQ_BW, d.bw_oct),
            ..b
        }),
    }
}

pub fn cap_input(s: &InputState) -> InputState {
    InputState {
        trim_db: fix(s.trim_db, TRIM_DB, 0.0),
        eq: cap_eq(&s.eq),
        ..*s
    }
}

pub fn cap_level(s: &Level) -> Level {
    Level {
        gain_db: fix(s.gain_db, FADER_DB, DB_OFF),
        pan: fix(s.pan, PAN, 0.0),
        ..*s
    }
}

pub fn cap_group(s: &MixGroup) -> MixGroup {
    MixGroup {
        gain_db: fix(s.gain_db, FADER_DB, 0.0),
        eq: cap_eq(&s.eq),
        ..*s
    }
}

pub fn cap_out(s: &MixOut) -> MixOut {
    let mut o = MixOut {
        volume_db: fix(s.volume_db, FADER_DB, 0.0),
        eq: cap_eq(&s.eq),
        ..*s
    };
    o.limiter.limit_db = fix(s.limiter.limit_db, LIMIT_DB, LIMIT_DB.0);
    o
}

/// An id cut to 64 bytes for a message (a char boundary kept).
pub fn clip(s: &str) -> &str {
    let mut end = s.len().min(64);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or_default()
}

/// `state` shaped by `topo`: every input, and in every mix a level for every
/// input, for each mix it hears and each peer, and a strip for every group,
/// known values capped and missing ones defaulted; returns what the topology
/// does not have, named (inputs, then mix by mix, then unknown mixes).
pub fn reconcile(topo: &TopologyInfo, state: &MixState) -> (MixState, Vec<String>) {
    let known_input = |id: &InputId| topo.inputs.iter().any(|i| i.id == *id);
    let known_group = |id: &GroupId| topo.groups.iter().any(|g| g.id == *id);
    let known_mix = |id: &MixId| topo.mixes.iter().any(|m| m.id == *id);
    let inputs = topo
        .inputs
        .iter()
        .map(|n| {
            let s = state.inputs.get(&n.id).map(cap_input).unwrap_or_default();
            (n.id.clone(), s)
        })
        .collect();
    let mut dropped: Vec<String> = state
        .inputs
        .keys()
        .filter(|id| !known_input(id))
        .map(|id| format!("input {}", clip(&id.0)))
        .collect();
    let mut mixes = BTreeMap::new();
    for node in &topo.mixes {
        let given = state.mixes.get(&node.id);
        let mut mix = Mix {
            out: given.map(|x| cap_out(&x.out)).unwrap_or_default(),
            ..Mix::default()
        };
        for i in &topo.inputs {
            let level = given.and_then(|x| x.inputs.get(&i.id));
            mix.inputs
                .insert(i.id.clone(), level.map(cap_level).unwrap_or_default());
        }
        // The mixes it hears, then its peers (#58): both are `Mix::mixes`.
        for h in node.mixes.iter().chain(&node.peers) {
            let level = given.and_then(|x| x.mixes.get(h));
            mix.mixes
                .insert(h.clone(), level.map(cap_level).unwrap_or_default());
        }
        for g in &topo.groups {
            let strip = given.and_then(|x| x.groups.get(&g.id));
            mix.groups
                .insert(g.id.clone(), strip.map(cap_group).unwrap_or_default());
        }
        if let Some(x) = given {
            let name = clip(&node.id.0);
            let hears = |id: &MixId| node.mixes.contains(id) || node.peers.contains(id);
            dropped.extend(
                x.inputs
                    .keys()
                    .filter(|id| !known_input(id))
                    .map(|id| format!("mix {name} input {}", clip(&id.0))),
            );
            dropped.extend(
                x.groups
                    .keys()
                    .filter(|id| !known_group(id))
                    .map(|id| format!("mix {name} group {}", clip(&id.0))),
            );
            dropped.extend(
                x.mixes
                    .keys()
                    .filter(|id| !hears(id))
                    .map(|id| format!("mix {name} hearing {}", clip(&id.0))),
            );
        }
        mixes.insert(node.id.clone(), mix);
    }
    dropped.extend(
        state
            .mixes
            .keys()
            .filter(|id| !known_mix(id))
            .map(|id| format!("mix {}", clip(&id.0))),
    );
    (MixState { inputs, mixes }, dropped)
}

#[cfg(test)]
mod tests;
