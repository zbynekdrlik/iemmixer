//! Synthetic projects in the predecessor's shape (S4, #20 design note §7):
//! the public tests' stand-in for the real site project (P6).
//! `synthetic_site` is the program spec §3.1 topology with the synthetic ids
//! and channels of `config/test-site.toml`, `synthetic_routing` the
//! predecessor's REAPER routing for it, and `project` writes a topology and a
//! state as the predecessor's REAPER project would hold them.

use std::collections::BTreeMap;

use iem_engine_proto::{
    BandKind as EqKind, Eq as EqSettings, EqBand, GroupId, InputId, InputState, Level, Limiter,
    Mix, MixGroup, MixId, MixOut, MixState, Source, db_to_lin,
};

use crate::aliases::MemberAlias;
use crate::fx::guid;
use crate::import::rea_kind;
use crate::reaeq::{Band, ReaEq};
use crate::rpp::{Chunk, RppError, num, q};
use crate::topology::{Routing, TopoGroup, TopoInput, TopoMix, Topology};

const DIRECT: [&str; 17] = [
    "mic1", "mic2", "mic3", "mic4", "mic5", "mic6", "mic7", "mic8", "mic9", "mic10", "hand1",
    "hand2", "hand3", "eng_mic", "keys", "iemonly", "content",
];
const STEMS_GROUP: [&str; 7] = ["click", "guide", "drums", "bass", "inst", "other", "bgvs"];
const STEREO: [&str; 8] = [
    "keys", "iemonly", "content", "drums", "bass", "inst", "other", "bgvs",
];
/// The input the translator hears in the predecessor (program spec §3.1).
const TRANSLATED: &str = "hand1";

/// The §3.1 topology with the ids and channels of `config/test-site.toml`,
/// the mixes in the predecessor's track order.
pub fn synthetic_site() -> Topology {
    let mut t = Topology::default();
    let mut rx = 101u16;
    for id in DIRECT.iter().chain(&STEMS_GROUP) {
        let n = if STEREO.contains(id) { 2 } else { 1 };
        t.inputs.push(TopoInput {
            id: InputId::new(*id),
            rx: (rx..rx + n).collect(),
            talkback: *id == "eng_mic",
        });
        rx += n;
    }
    t.groups.push(TopoGroup {
        id: GroupId::new("stems"),
        inputs: STEMS_GROUP.iter().map(|i| InputId::new(*i)).collect(),
    });
    let members: Vec<MixId> = (1..=9).map(|n| MixId::new(format!("member{n}"))).collect();
    for (n, m) in (1u16..).zip(&members) {
        t.mixes.push(TopoMix {
            id: m.clone(),
            tx: vec![69 + 2 * n, 70 + 2 * n],
            mixes: if n == 1 {
                members.iter().skip(1).cloned().collect()
            } else {
                Vec::new()
            },
        });
    }
    t.mixes.push(TopoMix {
        id: MixId::new("engineer"),
        tx: vec![91, 92],
        mixes: members.clone(),
    });
    t.mixes.push(TopoMix {
        id: MixId::new("translator"),
        tx: vec![93],
        mixes: Vec::new(),
    });
    t.engineer = Some(MixId::new("engineer"));
    t
}

/// The predecessor's routing for a topology of the §3.1 shape: a stereo mix
/// receives every ungrouped input and the mixes it hears and has an instance
/// of every group, which receives the group's inputs; a mono mix (the
/// translator) receives only `hand1`.
pub fn synthetic_routing(topo: &Topology) -> Routing {
    let mut r = Routing::default();
    for m in &topo.mixes {
        if m.tx.len() != 2 {
            let hand1 = InputId::new(TRANSLATED);
            if topo.input(&hand1).is_some() {
                r.levels.insert((m.id.clone(), Source::Input(hand1)));
            }
            continue;
        }
        for i in &topo.inputs {
            r.levels.insert((m.id.clone(), Source::Input(i.id.clone())));
        }
        for h in &m.mixes {
            r.levels.insert((m.id.clone(), Source::Mix(h.clone())));
        }
        for g in &topo.groups {
            r.strips.insert((m.id.clone(), g.id.clone()));
        }
    }
    r
}

fn random_eq(r: &mut dyn FnMut() -> f64) -> EqSettings {
    let mut e = EqSettings {
        gain_db: (r() * 12.0 - 6.0).round(),
        ..EqSettings::default()
    };
    for (i, b) in e.bands.iter_mut().enumerate() {
        let enabled = r() > 0.4;
        *b = EqBand {
            kind: [
                EqKind::HighPass,
                EqKind::LowShelf,
                EqKind::Peak,
                EqKind::HighShelf,
            ][i % 4],
            enabled,
            freq_hz: 40.0 + r() * 12_000.0,
            gain_db: if i == 2 { -150.0 } else { r() * 24.0 - 12.0 },
            bw_oct: 0.1 + r() * 3.0,
        };
    }
    e
}

/// A deterministic state over what `routing` holds in `topo` (see
/// `import::project`), in which every kind of value differs from its default
/// within the engine's caps; `seed` varies it.
pub fn sample_state(topo: &Topology, routing: &Routing, seed: u64) -> MixState {
    let mut k = seed;
    let mut next = move || {
        k = k
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (k >> 11) as f64 / (1u64 << 53) as f64
    };
    let mut s = MixState::default();
    for i in &topo.inputs {
        let e = random_eq(&mut next);
        s.inputs.insert(
            i.id.clone(),
            InputState {
                trim_db: next() * 30.0 - 10.0,
                muted: next() > 0.8,
                processing: next() > 0.2,
                eq: e,
            },
        );
    }
    for m in &topo.mixes {
        let e = random_eq(&mut next);
        let mut out = MixOut {
            volume_db: next() * 20.0 - 12.0,
            muted: next() > 0.7,
            ..MixOut::default()
        };
        if m.tx.len() == 2 {
            out.eq = e;
            out.limiter = Limiter {
                enabled: next() > 0.2,
                limit_db: -(next() * 6.0),
            };
        } else {
            out.limiter.enabled = false;
        }
        let mut mix = Mix {
            out,
            ..Mix::default()
        };
        for g in &topo.groups {
            if routing.has_strip(&m.id, &g.id) {
                let e = random_eq(&mut next);
                mix.groups.insert(
                    g.id.clone(),
                    MixGroup {
                        gain_db: next() * 20.0 - 12.0,
                        muted: next() > 0.7,
                        eq: e,
                    },
                );
            }
        }
        let sources = topo
            .inputs
            .iter()
            .map(|i| Source::Input(i.id.clone()))
            .chain(m.mixes.iter().map(|h| Source::Mix(h.clone())));
        for src in sources {
            if !routing.has_level(&m.id, &src) {
                continue;
            }
            let off = next() > 0.9;
            let level = Level {
                gain_db: if off { -150.0 } else { next() * 72.0 - 60.0 },
                pan: next() * 2.0 - 1.0,
                muted: next() > 0.7,
            };
            match src {
                Source::Input(i) => mix.inputs.insert(i, level),
                Source::Mix(h) => mix.mixes.insert(h, level),
            };
        }
        s.mixes.insert(m.id.clone(), mix);
    }
    // Every seed has an input without processing, a disabled limiter and a
    // level that is off.
    if let Some(i) = s.inputs.values_mut().next() {
        i.processing = false;
    }
    if let Some(m) = topo.mixes.iter().find(|m| m.tx.len() == 2)
        && let Some(x) = s.mixes.get_mut(&m.id)
    {
        x.out.limiter.enabled = false;
    }
    if let Some(l) = s
        .mixes
        .values_mut()
        .flat_map(|m| m.inputs.values_mut())
        .next()
    {
        l.gain_db = -150.0;
    }
    s
}

/// The REAPER track name of an id in synthetic projects.
pub fn track_name(id: &str) -> String {
    format!("{} trk", id.to_uppercase())
}

/// The id-like name of `group`'s instance in `mix` (its synthetic track is
/// `track_name` of it).
pub fn instance_name(mix: &MixId, group: &GroupId) -> String {
    format!("{mix}.{group}")
}

/// `aliases.toml` text for a synthetic project written with `track_name`.
pub fn aliases_toml(
    topo: &Topology,
    routing: &Routing,
    members: &BTreeMap<String, MemberAlias>,
) -> String {
    let mut s = String::from("[tracks]\n");
    let ids = topo
        .inputs
        .iter()
        .map(|i| i.id.0.as_str())
        .chain(topo.mixes.iter().map(|m| m.id.0.as_str()));
    for id in ids {
        s.push_str(&format!("\"{}\" = \"{id}\"\n", track_name(id)));
    }
    for (mix, group) in &routing.strips {
        s.push_str(&format!(
            "\"{}\" = \"{group}\"\n",
            track_name(&instance_name(mix, group))
        ));
    }
    s.push_str("\n[members]\n");
    for (legacy, m) in members {
        s.push_str(&format!(
            "{legacy} = {{ id = \"{}\", mix = \"{}\", archived = {} }}\n",
            m.id, m.mix, m.archived
        ));
    }
    s
}

fn reaeq(e: &EqSettings) -> ReaEq {
    ReaEq {
        bands: e
            .bands
            .iter()
            .map(|b| Band {
                kind: rea_kind(b.kind),
                enabled: b.enabled,
                freq_hz: b.freq_hz,
                gain_lin: db_to_lin(b.gain_db),
                bw_oct: b.bw_oct,
            })
            .collect(),
        global_gain: db_to_lin(e.gain_db),
    }
}

fn js(head: &str, sliders: &[f64]) -> Result<Chunk, RppError> {
    let mut fields = sliders
        .iter()
        .map(|v| num(*v))
        .collect::<Result<Vec<_>, _>>()?;
    fields.resize(64, "-".to_owned());
    let mut c = Chunk::new(head);
    c.line(fields.join(" "));
    Ok(c)
}

fn stand_in(name: &str) -> Chunk {
    let mut c = Chunk::new(format!(
        "VST \"VST3: {name} (Test)\" test.vst3 0 \"\" 1{{00}} \"\""
    ));
    c.line("AAAA");
    c
}

fn chain(seed: &str, plugins: Vec<(bool, Chunk)>) -> Chunk {
    let mut c = Chunk::new("FXCHAIN");
    c.line("SHOW 0").line("LASTSEL 0").line("DOCKED 0");
    for (i, (bypassed, chunk)) in plugins.into_iter().enumerate() {
        c.line(format!("BYPASS {} 0 0", u8::from(bypassed)));
        c.child(chunk);
        c.line("FLOATPOS 0 0 0 0");
        c.line(format!("FXID {}", guid(&format!("{seed}/fx{i}"))));
        c.line("WAK 0 0");
    }
    c
}

fn track_head(
    c: &mut Chunk,
    name: &str,
    fader_db: f64,
    pan: f64,
    muted: bool,
) -> Result<(), RppError> {
    c.line(format!("NAME {}", q(name)?));
    c.line("PEAKCOL 16576");
    c.line(format!(
        "VOLPAN {} {} -1 -1 1",
        num(db_to_lin(fader_db))?,
        num(pan)?
    ));
    c.line(format!("MUTESOLO {} 0 0", u8::from(muted)));
    c.line("IPHASE 0");
    Ok(())
}

fn receive(src: usize, mode: u8, l: &Level) -> Result<String, RppError> {
    Ok(format!(
        "AUXRECV {src} {mode} {} {} {} 0 0 0 0 -1:U 0 -1 ''",
        num(db_to_lin(l.gain_db))?,
        num(l.pan)?,
        u8::from(l.muted)
    ))
}

/// A project in the predecessor's shape: the muted master in the header;
/// inputs (record-armed at unity, TRIM, ReaEQ, the talkback stand-in on the
/// talkback input, a bypassed extra plug-in on the first input); mixes (one
/// hardware output, the receives `routing` holds, ReaEQ and the limiter and
/// the listen stand-in on the engineer's when stereo, no plug-ins when mono);
/// then every group instance (ReaEQ, its inputs' receives, feeding its mix at
/// unity). Channels map back as the importer reads them.
pub fn project(
    topo: &Topology,
    routing: &Routing,
    state: &MixState,
    name: &dyn Fn(&str) -> String,
) -> Result<String, RppError> {
    let bad = |m: String| RppError::Invalid(m);
    let mut p = Chunk::new(r#"REAPER_PROJECT 0.1 "7.65/win64" 0 0"#);
    p.line("RIPPLE 0 0")
        .line("PANLAW 1")
        .line("PANMODE 3")
        .line("SAMPLERATE 96000 1 0");
    p.line("MASTERMUTESOLO 1");
    p.line("MASTERHWOUT 88 0 1 0 0 0 0 -1");
    p.line("MASTER_NCH 2 2");
    p.line("MASTER_VOLUME 1 0 -1 -1 1");
    let instances: Vec<&(MixId, GroupId)> = routing.strips.iter().collect();
    let mut index: BTreeMap<String, usize> = BTreeMap::new();
    for (k, i) in topo.inputs.iter().enumerate() {
        index.insert(i.id.0.clone(), k);
    }
    for (k, m) in topo.mixes.iter().enumerate() {
        index.insert(m.id.0.clone(), topo.inputs.len() + k);
    }
    let first_instance = topo.inputs.len() + topo.mixes.len();
    let find = |id: &str| {
        index
            .get(id)
            .copied()
            .ok_or_else(|| bad(format!("{id} is not in the topology")))
    };
    for (k, i) in topo.inputs.iter().enumerate() {
        let s = state.inputs.get(&i.id).copied().unwrap_or_default();
        let seed = format!("site/{}", i.id);
        let mut c = Chunk::new(format!("TRACK {}", guid(&seed)));
        track_head(&mut c, &name(i.id.0.as_str()), 0.0, 0.0, s.muted)?;
        let rec = match i.rx.as_slice() {
            [a] => i64::from(*a) - 1,
            [a, _] => i64::from(*a) + 1023,
            _ => return Err(bad(format!("input {} has {} channels", i.id, i.rx.len()))),
        };
        c.line(format!("REC 1 {rec} 1 2 0 0 0 0"));
        c.line("NCHAN 2");
        c.line(format!("FX {}", u8::from(s.processing)));
        c.line(format!("TRACKID {}", guid(&seed)));
        c.line("MAINSEND 1 0");
        let mut plugins = vec![
            (
                false,
                js(r#"JS utility/volume_pan "TRIM IN""#, &[s.trim_db, 0.0, 0.0])?,
            ),
            (false, reaeq(&s.eq).chunk()?),
        ];
        if i.talkback {
            plugins.push((false, stand_in("OIEM Receive")));
        }
        if k == 0 {
            plugins.push((
                true,
                js(r#"JS synthesis/tonegenerator """#, &[-12.0, -6.0, 440.0])?,
            ));
        }
        c.child(chain(&seed, plugins));
        p.child(c);
    }
    for m in &topo.mixes {
        let s = state.mixes.get(&m.id).cloned().unwrap_or_default();
        let seed = format!("site/{}", m.id);
        let mut c = Chunk::new(format!("TRACK {}", guid(&seed)));
        track_head(
            &mut c,
            &name(m.id.0.as_str()),
            s.out.volume_db,
            0.0,
            s.out.muted,
        )?;
        c.line("REC 0 0 1 0 0 0 0 0");
        c.line("NCHAN 2");
        c.line("FX 1");
        c.line(format!("TRACKID {}", guid(&seed)));
        c.line("MAINSEND 0 0");
        match m.tx.as_slice() {
            [a, _] => c.line(format!("HWOUT {} 0 1 0 0 0 0 -1:U -1", a - 1)),
            [a] => c.line(format!(
                "HWOUT {} 0 1 0 0 0 0 -1:U -1",
                u32::from(*a) + 1023
            )),
            _ => return Err(bad(format!("mix {} has TX {:?}", m.id, m.tx))),
        };
        for i in &topo.inputs {
            let src = Source::Input(i.id.clone());
            if topo.group_of(&i.id).is_none() && routing.has_level(&m.id, &src) {
                let l = s.inputs.get(&i.id).copied().unwrap_or_default();
                c.line(receive(find(&i.id.0)?, 3, &l)?);
            }
        }
        for (k, (mix, _)) in instances.iter().enumerate() {
            if *mix == m.id {
                c.line(format!(
                    "AUXRECV {} 0 1 0 0 0 0 0 0 -1:U 0 -1 ''",
                    first_instance + k
                ));
            }
        }
        for h in &m.mixes {
            if routing.has_level(&m.id, &Source::Mix(h.clone())) {
                let l = s.mixes.get(h).copied().unwrap_or_default();
                c.line(receive(find(&h.0)?, 0, &l)?);
            }
        }
        let mut plugins = Vec::new();
        if m.tx.len() == 2 {
            plugins.push((false, reaeq(&s.out.eq).chunk()?));
            let l = s.out.limiter.limit_db;
            plugins.push((
                !s.out.limiter.enabled,
                js("JS loser/MGA_JSLimiterST LIMITER", &[l, 50.0, 75.0, l, 0.0])?,
            ));
            if topo.engineer.as_ref() == Some(&m.id) {
                plugins.push((false, stand_in("VBAN IEM")));
            }
        }
        c.child(chain(&seed, plugins));
        p.child(c);
    }
    for (mix, group) in &instances {
        let strip = state
            .mixes
            .get(mix)
            .and_then(|x| x.groups.get(group))
            .copied()
            .unwrap_or_default();
        let levels = state.mixes.get(mix).map(|x| &x.inputs);
        let id = instance_name(mix, group);
        let seed = format!("site/{id}");
        let mut c = Chunk::new(format!("TRACK {}", guid(&seed)));
        track_head(&mut c, &name(&id), strip.gain_db, 0.0, strip.muted)?;
        c.line("REC 0 0 1 0 0 0 0 0");
        c.line("NCHAN 2");
        c.line("FX 1");
        c.line(format!("TRACKID {}", guid(&seed)));
        c.line("MAINSEND 1 0");
        let members = topo
            .group(group)
            .ok_or_else(|| bad(format!("group {group} is not in the topology")))?;
        for i in &members.inputs {
            if routing.has_level(mix, &Source::Input(i.clone())) {
                let l = levels.and_then(|x| x.get(i)).copied().unwrap_or_default();
                c.line(receive(find(&i.0)?, 3, &l)?);
            }
        }
        c.child(chain(&seed, vec![(false, reaeq(&strip.eq).chunk()?)]));
        p.child(c);
    }
    Ok(p.render())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aliases::parse_aliases;
    use crate::import::{compare, import, project as projection};
    use crate::legacy::LegacyProject;
    use crate::topology::Counts;

    #[test]
    fn a_synthetic_project_imports_to_its_topology_routing_and_state() {
        let topo = synthetic_site();
        let routing = synthetic_routing(&topo);
        let state = sample_state(&topo, &routing, 7);
        let text = project(&topo, &routing, &state, &track_name).unwrap();
        let aliases = parse_aliases(&aliases_toml(&topo, &routing, &BTreeMap::new())).unwrap();
        let p = LegacyProject::parse(&text).unwrap();
        assert_eq!(p.tracks.len(), 45);
        let imp = import(&p, &aliases).unwrap();
        assert!(
            imp.topology.diff(&topo).is_empty(),
            "{:#?}",
            imp.topology.diff(&topo)
        );
        assert_eq!(imp.routing, routing);
        assert_eq!(
            imp.counts,
            Counts {
                tracks: 45,
                sends: 268,
                eqs: 44,
                limiters: 10,
                trims: 24
            }
        );
        assert_eq!(
            compare(&topo, &routing, &imp.state, &state, 1e-9),
            Vec::<String>::new()
        );
        assert_eq!(imp.state, projection(&topo, &routing, &imp.state));
        assert_eq!(imp.notes.len(), 1, "{:?}", imp.notes);
        assert!(imp.notes[0].contains("synthesis/tonegenerator"));
    }

    #[test]
    fn the_synthetic_routing_is_the_predecessors() {
        let topo = synthetic_site();
        let r = synthetic_routing(&topo);
        // 10 stereo mixes × 24 inputs, 17 heard mixes, the translator's hand1.
        assert_eq!(r.levels.len(), 10 * 24 + 17 + 1);
        assert_eq!(r.strips.len(), 10);
        let tr = MixId::new("translator");
        assert!(r.has_level(&tr, &Source::Input(InputId::new("hand1"))));
        assert!(!r.has_level(&tr, &Source::Input(InputId::new("mic1"))));
        assert!(!r.has_strip(&tr, &GroupId::new("stems")));
        let m1 = MixId::new("member1");
        assert!(r.has_level(&m1, &Source::Mix(MixId::new("member9"))));
        assert!(r.has_level(&m1, &Source::Input(InputId::new("drums"))));
        assert!(!r.has_level(&MixId::new("member2"), &Source::Mix(m1)));
    }

    #[test]
    fn sample_states_vary_with_the_seed_and_stay_in_caps() {
        let topo = synthetic_site();
        let routing = synthetic_routing(&topo);
        let a = sample_state(&topo, &routing, 1);
        assert_ne!(a, sample_state(&topo, &routing, 2));
        assert_eq!(a, sample_state(&topo, &routing, 1));
        let levels: Vec<&Level> = a
            .mixes
            .values()
            .flat_map(|m| m.inputs.values().chain(m.mixes.values()))
            .collect();
        assert_eq!(levels.len(), 258);
        assert!(levels.iter().any(|l| l.gain_db <= -150.0));
        assert!(levels.iter().all(|l| l.gain_db <= 12.0));
        assert!(a.inputs.values().any(|i| !i.processing));
        assert!(a.mixes.values().any(|m| !m.out.limiter.enabled));
        for m in a.mixes.values() {
            assert!((-6.0..=0.0).contains(&m.out.limiter.limit_db));
            assert!(m.out.volume_db <= 12.0);
        }
        let tr = &a.mixes[&MixId::new("translator")];
        assert!(!tr.out.limiter.enabled);
        assert_eq!(tr.out.eq, EqSettings::default());
        assert!(tr.groups.is_empty());
        assert_eq!(a.mixes[&MixId::new("member1")].groups.len(), 1);
        // Mix mutes and off levels are drawn, not only forced (values
        // computed from the generator's definition): 3 muted mixes, and 26
        // levels drawn off besides the one set off.
        let muted: Vec<&str> = a
            .mixes
            .iter()
            .filter(|(_, m)| m.out.muted)
            .map(|(id, _)| id.0.as_str())
            .collect();
        assert_eq!(muted, ["engineer", "member2", "member6"]);
        assert_eq!(levels.iter().filter(|l| l.gain_db <= -150.0).count(), 27);
    }

    #[test]
    fn projects_refuse_what_they_cannot_hold() {
        let routing = synthetic_routing(&synthetic_site());
        let state = sample_state(&synthetic_site(), &routing, 3);
        let mut topo = synthetic_site();
        topo.inputs[0].rx = vec![1, 2, 3];
        assert!(project(&topo, &routing, &state, &track_name).is_err());
        let mut topo = synthetic_site();
        topo.mixes[0].tx = vec![1, 2, 3];
        assert!(project(&topo, &routing, &state, &track_name).is_err());
        let topo = synthetic_site();
        let mut r = routing.clone();
        r.levels
            .insert((MixId::new("member1"), Source::Mix(MixId::new("nope"))));
        let mut t = topo.clone();
        t.mixes[0].mixes.push(MixId::new("nope"));
        assert!(project(&t, &r, &state, &track_name).is_err());
        let mut r = routing;
        r.strips
            .insert((MixId::new("member1"), GroupId::new("nope")));
        assert!(project(&topo, &r, &state, &track_name).is_err());
    }

    #[test]
    fn synthetic_mixes_use_the_test_site_channels() {
        let t = synthetic_site();
        let tx: Vec<(&str, Vec<u16>)> = t
            .mixes
            .iter()
            .map(|m| (m.id.0.as_str(), m.tx.clone()))
            .collect();
        let want: Vec<(&str, Vec<u16>)> = vec![
            ("member1", vec![71, 72]),
            ("member2", vec![73, 74]),
            ("member3", vec![75, 76]),
            ("member4", vec![77, 78]),
            ("member5", vec![79, 80]),
            ("member6", vec![81, 82]),
            ("member7", vec![83, 84]),
            ("member8", vec![85, 86]),
            ("member9", vec![87, 88]),
            ("engineer", vec![91, 92]),
            ("translator", vec![93]),
        ];
        assert_eq!(tx, want);
        assert_eq!(t.mixes[0].mixes.len(), 8);
        assert_eq!(t.mixes[9].mixes.len(), 9);
        assert_eq!(t.groups[0].inputs.len(), 7);
        assert_eq!(t.engineer, Some(MixId::new("engineer")));
    }

    #[test]
    fn the_first_input_carries_a_bypassed_tone_generator_and_the_master_is_muted() {
        let topo = synthetic_site();
        let routing = synthetic_routing(&topo);
        let text = project(
            &topo,
            &routing,
            &sample_state(&topo, &routing, 7),
            &track_name,
        )
        .unwrap();
        assert_eq!(text.matches("tonegenerator").count(), 1);
        let lines: Vec<&str> = text.lines().collect();
        let at = lines
            .iter()
            .position(|l| l.contains("tonegenerator"))
            .unwrap();
        assert_eq!(lines[at - 1].trim(), "BYPASS 1 0 0");
        assert_eq!(lines[at].trim(), r#"<JS synthesis/tonegenerator """#);
        let sliders = format!("-12 -6 440{}", " -".repeat(61));
        assert_eq!(lines[at + 1].trim(), sliders);
        assert!(text.contains("\n  MASTERMUTESOLO 1\n"));
        assert_eq!(instance_name(&MixId::new("m"), &GroupId::new("g")), "m.g");
    }

    /// An EQ with the band kinds `random_eq` gives: (enabled, Hz, dB, octaves).
    fn golden_eq(gain_db: f64, bands: [(bool, f64, f64, f64); 5]) -> EqSettings {
        let kinds = [
            EqKind::HighPass,
            EqKind::LowShelf,
            EqKind::Peak,
            EqKind::HighShelf,
            EqKind::HighPass,
        ];
        let mut e = EqSettings {
            gain_db,
            ..EqSettings::default()
        };
        for (b, (kind, (enabled, freq_hz, gain_db, bw_oct))) in
            e.bands.iter_mut().zip(kinds.into_iter().zip(bands))
        {
            *b = EqBand {
                kind,
                enabled,
                freq_hz,
                gain_db,
                bw_oct,
            };
        }
        e
    }

    /// Every value of a small topology's sample state, pinned: the generator
    /// is the public tests' stand-in for the site, so a change to it must be
    /// deliberate (values computed from the generator's definition).
    #[test]
    fn sample_state_is_pinned_value_for_value() {
        let input = |i: &str, rx: u16| TopoInput {
            id: InputId::new(i),
            rx: vec![rx],
            talkback: false,
        };
        let mix = |m: &str, tx: Vec<u16>, heard: &[&str]| TopoMix {
            id: MixId::new(m),
            tx,
            mixes: heard.iter().map(|h| MixId::new(*h)).collect(),
        };
        let topo = Topology {
            inputs: vec![input("a", 1), input("b", 2), input("c", 3)],
            groups: vec![TopoGroup {
                id: GroupId::new("g"),
                inputs: vec![InputId::new("c")],
            }],
            mixes: vec![
                mix("out2", vec![3, 4], &[]),
                mix("out1", vec![1, 2], &["out2"]),
                mix("tr", vec![5], &[]),
            ],
            engineer: None,
        };
        let mut routing = Routing::default();
        for (m, i) in [
            ("out1", "a"),
            ("out1", "b"),
            ("out1", "c"),
            ("out2", "b"),
            ("tr", "a"),
        ] {
            routing
                .levels
                .insert((MixId::new(m), Source::Input(InputId::new(i))));
        }
        routing
            .levels
            .insert((MixId::new("out1"), Source::Mix(MixId::new("out2"))));
        routing
            .strips
            .insert((MixId::new("out1"), GroupId::new("g")));
        let mut want = MixState::default();
        want.inputs.insert(
            InputId::new("a"),
            InputState {
                trim_db: 7.860745325569493,
                muted: true,
                processing: false,
                eq: golden_eq(
                    4.0,
                    [
                        (
                            false,
                            9886.044151794627,
                            -4.573148693757647,
                            1.428206679367011,
                        ),
                        (
                            false,
                            5478.82485321425,
                            9.676293490464722,
                            0.9023689834282962,
                        ),
                        (false, 2387.418789928381, -150.0, 0.754046099943221),
                        (
                            true,
                            2392.262500215231,
                            1.5781712142400401,
                            2.976385130613216,
                        ),
                        (
                            false,
                            11582.863382255046,
                            -5.72233850346369,
                            1.2231254879589302,
                        ),
                    ],
                ),
            },
        );
        want.inputs.insert(
            InputId::new("b"),
            InputState {
                trim_db: -1.2040519771370999,
                muted: true,
                processing: true,
                eq: golden_eq(
                    -2.0,
                    [
                        (
                            true,
                            1581.2409648583675,
                            -6.174539722524267,
                            1.7076004895480819,
                        ),
                        (
                            true,
                            427.5343839112082,
                            -6.022011340477347,
                            3.065287244861955,
                        ),
                        (true, 281.2132548795927, -150.0, 0.10470576157791059),
                        (
                            false,
                            2770.076306626242,
                            5.692047239534784,
                            0.2738985359506254,
                        ),
                        (
                            false,
                            9677.049667116022,
                            -11.73719494152377,
                            0.3901552665017537,
                        ),
                    ],
                ),
            },
        );
        want.inputs.insert(
            InputId::new("c"),
            InputState {
                trim_db: -6.621520090429529,
                muted: false,
                processing: true,
                eq: golden_eq(
                    3.0,
                    [
                        (
                            true,
                            10097.000078715557,
                            -0.1549327673102301,
                            0.390466853682767,
                        ),
                        (
                            true,
                            3882.815203156875,
                            -8.386903782575477,
                            1.5795244724403958,
                        ),
                        (true, 1924.0233518485884, -150.0, 0.22347864603391868),
                        (
                            false,
                            6411.503766339949,
                            -3.09594962105016,
                            2.7735297299149155,
                        ),
                        (
                            true,
                            10912.60490330623,
                            -7.865116961622644,
                            1.93032652129178,
                        ),
                    ],
                ),
            },
        );
        let mut out2 = Mix {
            out: MixOut {
                volume_db: -10.201552614400986,
                muted: false,
                eq: golden_eq(
                    -1.0,
                    [
                        (
                            true,
                            3337.518449803024,
                            11.911862843979627,
                            1.328587472597789,
                        ),
                        (
                            true,
                            6171.43354348855,
                            -5.697084940225904,
                            2.5255217544499944,
                        ),
                        (false, 7073.879016579883, -150.0, 2.7099804432343926),
                        (
                            true,
                            10469.253804599315,
                            3.3412217050192687,
                            0.10799098169480151,
                        ),
                        (
                            true,
                            240.37960695512248,
                            -0.15207232957601313,
                            1.8583872784453,
                        ),
                    ],
                ),
                limiter: Limiter {
                    enabled: false,
                    limit_db: -4.35950678129451,
                },
            },
            ..Mix::default()
        };
        out2.inputs.insert(
            InputId::new("b"),
            Level {
                gain_db: -0.7955934108683991,
                pan: 0.4495521336750077,
                muted: false,
            },
        );
        want.mixes.insert(MixId::new("out2"), out2);
        let mut out1 = Mix {
            out: MixOut {
                volume_db: -4.414614413689444,
                muted: false,
                eq: golden_eq(
                    3.0,
                    [
                        (
                            true,
                            5836.785292528361,
                            0.24417464270893063,
                            1.1006820919639724,
                        ),
                        (
                            true,
                            4197.287126456556,
                            -3.5419792311353255,
                            1.435953916032347,
                        ),
                        (true, 11924.589499107222, -150.0, 1.4402821252744298),
                        (
                            true,
                            362.99649068234925,
                            8.507452279250447,
                            0.31684116004784313,
                        ),
                        (
                            false,
                            3852.947712649576,
                            -3.302738049936046,
                            2.186791679218299,
                        ),
                    ],
                ),
                limiter: Limiter {
                    enabled: true,
                    limit_db: -0.9383011511582153,
                },
            },
            ..Mix::default()
        };
        out1.groups.insert(
            GroupId::new("g"),
            MixGroup {
                gain_db: 4.767233006947041,
                muted: true,
                eq: golden_eq(
                    -6.0,
                    [
                        (
                            false,
                            4585.781760815448,
                            5.805893217906906,
                            0.5982765293665356,
                        ),
                        (
                            true,
                            765.3514794650102,
                            -10.692908852337979,
                            2.8197269010741532,
                        ),
                        (false, 10258.236684863237, -150.0, 2.8801707883206205),
                        (
                            false,
                            1893.1325492056992,
                            -1.2654140995116627,
                            0.6789130628655704,
                        ),
                        (
                            true,
                            5760.929344855779,
                            11.159369543006008,
                            1.5455577584036342,
                        ),
                    ],
                ),
            },
        );
        out1.inputs.insert(
            InputId::new("a"),
            Level {
                gain_db: -150.0,
                pan: 0.4184026095972644,
                muted: false,
            },
        );
        out1.inputs.insert(
            InputId::new("b"),
            Level {
                gain_db: -56.72250935947566,
                pan: -0.51653629493709,
                muted: true,
            },
        );
        out1.inputs.insert(
            InputId::new("c"),
            Level {
                gain_db: -26.925410759704747,
                pan: -0.9614036318125088,
                muted: true,
            },
        );
        out1.mixes.insert(
            MixId::new("out2"),
            Level {
                gain_db: -19.491270429328857,
                pan: -0.9887505464729192,
                muted: false,
            },
        );
        want.mixes.insert(MixId::new("out1"), out1);
        let mut tr = Mix {
            out: MixOut {
                volume_db: 5.89271240134401,
                muted: false,
                eq: EqSettings::default(),
                limiter: Limiter {
                    enabled: false,
                    limit_db: -6.0,
                },
            },
            ..Mix::default()
        };
        tr.inputs.insert(
            InputId::new("a"),
            Level {
                gain_db: -32.93793984812034,
                pan: 0.8861208868323762,
                muted: false,
            },
        );
        want.mixes.insert(MixId::new("tr"), tr);

        assert_eq!(sample_state(&topo, &routing, 5), want);
    }
}
