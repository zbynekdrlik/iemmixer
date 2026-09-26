//! Synthetic projects in the predecessor's shape (S4): the public tests'
//! stand-in for the real site project (P6). `synthetic_site` is the program
//! spec §3.1 topology with the synthetic ids and channels of
//! `config/test-site.toml`; `project` writes a topology and a state as the
//! predecessor's REAPER project would hold them.

use std::collections::BTreeMap;

use iem_engine_proto::{
    BandKind as EqKind, BusId, BusKind, BusState, Eq as EqSettings, EqBand, InputId, InputState,
    Limiter, MixState, SendEntry, SendId, SendState, Source, Tap, db_to_lin,
};

use crate::aliases::MemberAlias;
use crate::fx::guid;
use crate::import::rea_kind;
use crate::reaeq::{Band, ReaEq};
use crate::rpp::{Chunk, RppError, num, q};
use crate::topology::{TopoBus, TopoInput, Topology};

const DIRECT: [&str; 17] = [
    "mic1", "mic2", "mic3", "mic4", "mic5", "mic6", "mic7", "mic8", "mic9", "mic10", "hand1",
    "hand2", "hand3", "eng_mic", "keys", "iemonly", "content",
];
const STEMS_GROUP: [&str; 7] = ["click", "guide", "drums", "bass", "inst", "other", "bgvs"];
const STEREO: [&str; 8] = [
    "keys", "iemonly", "content", "drums", "bass", "inst", "other", "bgvs",
];

/// The §3.1 topology with the ids and channels of `config/test-site.toml`.
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
    let members: Vec<String> = (1..=9)
        .map(|n| format!("member{n}"))
        .chain(["engineer".to_owned()])
        .collect();
    for (n, m) in (1u16..).zip(&members) {
        let first = if m == "engineer" { 91 } else { 69 + 2 * n };
        t.buses.push(TopoBus {
            id: BusId::new(m.clone()),
            kind: BusKind::Output,
            tx: vec![first, first + 1],
        });
    }
    for m in &members {
        t.buses.push(TopoBus {
            id: BusId::new(format!("{m}.stems")),
            kind: BusKind::Stems,
            tx: Vec::new(),
        });
    }
    t.buses.push(TopoBus {
        id: BusId::new("translator"),
        kind: BusKind::Translator,
        tx: vec![93],
    });
    t.buses.push(TopoBus {
        id: BusId::new("master"),
        kind: BusKind::Master,
        tx: vec![89, 90],
    });
    let mut add = |src: Source, dst: String, tap: Tap| {
        t.sends.push((
            SendId {
                src,
                dst: BusId::new(dst),
            },
            tap,
        ));
    };
    for i in DIRECT {
        for m in &members {
            add(Source::Input(InputId::new(i)), m.clone(), Tap::Pre);
        }
    }
    for i in STEMS_GROUP {
        for m in &members {
            add(
                Source::Input(InputId::new(i)),
                format!("{m}.stems"),
                Tap::Pre,
            );
        }
    }
    add(
        Source::Input(InputId::new("hand1")),
        "translator".into(),
        Tap::Pre,
    );
    for m in &members {
        add(
            Source::Bus(BusId::new(format!("{m}.stems"))),
            m.clone(),
            Tap::Post,
        );
    }
    for m in members.iter().skip(1).take(8) {
        add(
            Source::Bus(BusId::new(m.clone())),
            "member1".into(),
            Tap::Post,
        );
    }
    for m in members.iter().take(9) {
        add(
            Source::Bus(BusId::new(m.clone())),
            "engineer".into(),
            Tap::Post,
        );
    }
    t.engineer = Some(BusId::new("engineer"));
    t
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

/// A deterministic state over `topo` in which every kind of value differs
/// from its default (within the engine's caps); `seed` varies it.
pub fn sample_state(topo: &Topology, seed: u64) -> MixState {
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
                fader_db: next() * 20.0 - 10.0,
                pan: next() * 2.0 - 1.0,
                eq: e,
            },
        );
    }
    for b in &topo.buses {
        let e = random_eq(&mut next);
        let mut v = BusState {
            fader_db: next() * 20.0 - 12.0,
            pan: next() * 2.0 - 1.0,
            muted: next() > 0.7,
            ..BusState::default()
        };
        if matches!(b.kind, BusKind::Output | BusKind::Stems) {
            v.eq = e;
        }
        if b.kind == BusKind::Output {
            v.limiter = Limiter {
                enabled: next() > 0.2,
                limit_db: -(next() * 6.0),
            };
        }
        s.buses.insert(b.id.clone(), v);
    }
    for (id, _) in &topo.sends {
        let off = next() > 0.9;
        s.sends.push(SendEntry {
            id: id.clone(),
            state: SendState {
                gain_db: if off { -150.0 } else { next() * 72.0 - 60.0 },
                pan: next() * 2.0 - 1.0,
                muted: next() > 0.7,
            },
        });
    }
    s.sends.sort_by(|a, b| a.id.cmp(&b.id));
    // Every seed has an input without processing, a disabled limiter and a
    // send that is off.
    if let Some(i) = s.inputs.values_mut().next() {
        i.processing = false;
    }
    if let Some(b) = topo.buses.iter().find(|b| b.kind == BusKind::Output)
        && let Some(v) = s.buses.get_mut(&b.id)
    {
        v.limiter.enabled = false;
    }
    if let Some(e) = s.sends.first_mut() {
        e.state.gain_db = -150.0;
    }
    s
}

/// The REAPER track name of an id in synthetic projects.
pub fn track_name(id: &str) -> String {
    format!("{} trk", id.to_uppercase())
}

/// `aliases.toml` text for a synthetic project written with `track_name`.
pub fn aliases_toml(topo: &Topology, members: &BTreeMap<String, MemberAlias>) -> String {
    let mut s = String::new();
    if let Some(m) = topo.buses.iter().find(|b| b.kind == BusKind::Master) {
        s.push_str(&format!("master = \"{}\"\n", m.id));
    }
    s.push_str("\n[tracks]\n");
    let ids = topo.inputs.iter().map(|i| i.id.0.as_str()).chain(
        topo.buses
            .iter()
            .filter(|b| b.kind != BusKind::Master)
            .map(|b| b.id.0.as_str()),
    );
    for id in ids {
        s.push_str(&format!("\"{}\" = \"{id}\"\n", track_name(id)));
    }
    s.push_str("\n[members]\n");
    for (legacy, m) in members {
        s.push_str(&format!(
            "{legacy} = {{ id = \"{}\", bus = \"{}\", stems = \"{}\", archived = {} }}\n",
            m.id, m.bus, m.stems, m.archived
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

/// A project in the predecessor's shape: inputs (record-armed, TRIM, ReaEQ,
/// the talkback stand-in on the talkback input, a bypassed extra plug-in on
/// the first input), then every bus but the master (receives, one hardware
/// output, ReaEQ and limiter, the listen stand-in on the engineer bus), and
/// the master in the project header. Channels map back as the importer reads
/// them (design note §3.2).
pub fn project(
    topo: &Topology,
    state: &MixState,
    name: &dyn Fn(&str) -> String,
) -> Result<String, RppError> {
    let bad = |m: String| RppError::Invalid(m);
    let mut p = Chunk::new(r#"REAPER_PROJECT 0.1 "7.65/win64" 0 0"#);
    p.line("RIPPLE 0 0")
        .line("PANLAW 1")
        .line("PANMODE 3")
        .line("SAMPLERATE 96000 1 0");
    let master = topo
        .buses
        .iter()
        .find(|b| b.kind == BusKind::Master)
        .ok_or_else(|| bad("no master bus".into()))?;
    let ms = state.buses.get(&master.id).copied().unwrap_or_default();
    p.line(format!("MASTERMUTESOLO {}", u8::from(ms.muted)));
    let first = master
        .tx
        .first()
        .ok_or_else(|| bad("master without TX".into()))?;
    p.line(format!("MASTERHWOUT {} 0 1 0 0 0 0 -1", first - 1));
    p.line("MASTER_NCH 2 2");
    p.line(format!(
        "MASTER_VOLUME {} {} -1 -1 1",
        num(db_to_lin(ms.fader_db))?,
        num(ms.pan)?
    ));
    let mut index: BTreeMap<Source, usize> = BTreeMap::new();
    for (k, i) in topo.inputs.iter().enumerate() {
        index.insert(Source::Input(i.id.clone()), k);
    }
    let buses: Vec<&TopoBus> = topo
        .buses
        .iter()
        .filter(|b| b.kind != BusKind::Master)
        .collect();
    for (k, b) in buses.iter().enumerate() {
        index.insert(Source::Bus(b.id.clone()), topo.inputs.len() + k);
    }
    for (k, i) in topo.inputs.iter().enumerate() {
        let s = state.inputs.get(&i.id).copied().unwrap_or_default();
        let seed = format!("site/{}", i.id);
        let mut c = Chunk::new(format!("TRACK {}", guid(&seed)));
        track_head(&mut c, &name(i.id.0.as_str()), s.fader_db, s.pan, s.muted)?;
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
    for b in &buses {
        let s = state.buses.get(&b.id).copied().unwrap_or_default();
        let seed = format!("site/{}", b.id);
        let mut c = Chunk::new(format!("TRACK {}", guid(&seed)));
        track_head(&mut c, &name(b.id.0.as_str()), s.fader_db, s.pan, s.muted)?;
        c.line("REC 0 0 1 0 0 0 0 0");
        c.line("NCHAN 2");
        c.line("FX 1");
        c.line(format!("TRACKID {}", guid(&seed)));
        c.line(format!("MAINSEND {} 0", u8::from(b.kind == BusKind::Stems)));
        match (b.kind, b.tx.as_slice()) {
            (BusKind::Output, [a, _]) => {
                c.line(format!("HWOUT {} 0 1 0 0 0 0 -1:U -1", a - 1));
            }
            (BusKind::Translator, [a]) => {
                c.line(format!(
                    "HWOUT {} 0 1 0 0 0 0 -1:U -1",
                    u32::from(*a) + 1023
                ));
            }
            (BusKind::Stems, []) => {}
            _ => return Err(bad(format!("bus {} has TX {:?}", b.id, b.tx))),
        }
        for (id, tap) in topo.sends.iter().filter(|(id, _)| id.dst == b.id) {
            let src = index
                .get(&id.src)
                .ok_or_else(|| bad(format!("send {id} from an unknown source")))?;
            let st = state
                .sends
                .iter()
                .find(|e| e.id == *id)
                .map(|e| e.state)
                .unwrap_or_default();
            c.line(format!(
                "AUXRECV {src} {} {} {} {} 0 0 0 0 -1:U 0 -1 ''",
                if *tap == Tap::Pre { 3 } else { 0 },
                num(db_to_lin(st.gain_db))?,
                num(st.pan)?,
                u8::from(st.muted)
            ));
        }
        let mut plugins = Vec::new();
        if matches!(b.kind, BusKind::Output | BusKind::Stems) {
            plugins.push((false, reaeq(&s.eq).chunk()?));
        }
        if b.kind == BusKind::Output {
            let l = s.limiter.limit_db;
            plugins.push((
                !s.limiter.enabled,
                js("JS loser/MGA_JSLimiterST LIMITER", &[l, 50.0, 75.0, l, 0.0])?,
            ));
            if topo.engineer.as_ref() == Some(&b.id) {
                plugins.push((false, stand_in("VBAN IEM")));
            }
        }
        c.child(chain(&seed, plugins));
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

    #[test]
    fn a_synthetic_project_imports_to_its_topology_and_state() {
        let topo = synthetic_site();
        let state = sample_state(&topo, 7);
        let text = project(&topo, &state, &track_name).unwrap();
        let aliases = parse_aliases(&aliases_toml(&topo, &BTreeMap::new())).unwrap();
        let p = LegacyProject::parse(&text).unwrap();
        assert_eq!(p.tracks.len(), 45);
        let imp = import(&p, &aliases).unwrap();
        assert!(
            imp.topology.diff(&topo).is_empty(),
            "{:#?}",
            imp.topology.diff(&topo)
        );
        assert_eq!(imp.counts, topo.counts());
        assert_eq!(
            compare(&topo, &imp.state, &state, 1e-9),
            Vec::<String>::new()
        );
        assert_eq!(imp.state, projection(&topo, &imp.state));
        assert_eq!(imp.notes.len(), 1, "{:?}", imp.notes);
        assert!(imp.notes[0].contains("synthesis/tonegenerator"));
    }

    #[test]
    fn sample_states_vary_with_the_seed_and_stay_in_caps() {
        let topo = synthetic_site();
        let a = sample_state(&topo, 1);
        assert_ne!(a, sample_state(&topo, 2));
        assert_eq!(a, sample_state(&topo, 1));
        assert_eq!(a.sends.len(), 268);
        assert!(a.sends.iter().any(|e| e.state.gain_db <= -150.0));
        assert!(a.sends.iter().all(|e| e.state.gain_db <= 12.0));
        assert!(a.inputs.values().any(|i| !i.processing));
        assert!(a.buses.values().any(|b| !b.limiter.enabled));
        for b in a.buses.values() {
            assert!((-6.0..=0.0).contains(&b.limiter.limit_db));
        }
    }

    #[test]
    fn projects_refuse_what_they_cannot_hold() {
        let mut topo = synthetic_site();
        let state = sample_state(&topo, 3);
        topo.inputs[0].rx = vec![1, 2, 3];
        assert!(project(&topo, &state, &track_name).is_err());
        let mut topo = synthetic_site();
        topo.buses[0].tx = vec![1];
        assert!(project(&topo, &state, &track_name).is_err());
        let mut topo = synthetic_site();
        topo.buses.retain(|b| b.kind != BusKind::Master);
        assert!(project(&topo, &state, &track_name).is_err());
        let mut topo = synthetic_site();
        topo.sends.push((
            SendId {
                src: Source::Input(InputId::new("nope")),
                dst: BusId::new("member1"),
            },
            Tap::Pre,
        ));
        assert!(project(&topo, &state, &track_name).is_err());
    }

    #[test]
    fn synthetic_buses_use_the_test_site_channels() {
        let t = synthetic_site();
        let tx: Vec<(&str, BusKind, Vec<u16>)> = t
            .buses
            .iter()
            .map(|b| (b.id.0.as_str(), b.kind, b.tx.clone()))
            .collect();
        let out = BusKind::Output;
        let stems = BusKind::Stems;
        let want: Vec<(&str, BusKind, Vec<u16>)> = vec![
            ("member1", out, vec![71, 72]),
            ("member2", out, vec![73, 74]),
            ("member3", out, vec![75, 76]),
            ("member4", out, vec![77, 78]),
            ("member5", out, vec![79, 80]),
            ("member6", out, vec![81, 82]),
            ("member7", out, vec![83, 84]),
            ("member8", out, vec![85, 86]),
            ("member9", out, vec![87, 88]),
            ("engineer", out, vec![91, 92]),
            ("member1.stems", stems, vec![]),
            ("member2.stems", stems, vec![]),
            ("member3.stems", stems, vec![]),
            ("member4.stems", stems, vec![]),
            ("member5.stems", stems, vec![]),
            ("member6.stems", stems, vec![]),
            ("member7.stems", stems, vec![]),
            ("member8.stems", stems, vec![]),
            ("member9.stems", stems, vec![]),
            ("engineer.stems", stems, vec![]),
            ("translator", BusKind::Translator, vec![93]),
            ("master", BusKind::Master, vec![89, 90]),
        ];
        assert_eq!(tx, want);
    }

    #[test]
    fn the_first_input_carries_a_bypassed_tone_generator() {
        let topo = synthetic_site();
        let text = project(&topo, &sample_state(&topo, 7), &track_name).unwrap();
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
        let input = |i: &str| Source::Input(InputId::new(i));
        let bus = |b: &str| Source::Bus(BusId::new(b));
        let topo = Topology {
            inputs: ["a", "b", "c"]
                .into_iter()
                .map(|i| TopoInput {
                    id: InputId::new(i),
                    rx: vec![1],
                    talkback: false,
                })
                .collect(),
            buses: [
                ("out1", BusKind::Output),
                ("out2", BusKind::Output),
                ("st", BusKind::Stems),
                ("tr", BusKind::Translator),
                ("master", BusKind::Master),
            ]
            .into_iter()
            .map(|(b, kind)| TopoBus {
                id: BusId::new(b),
                kind,
                tx: Vec::new(),
            })
            .collect(),
            sends: [
                (input("a"), "out1"),
                (input("b"), "out2"),
                (input("c"), "st"),
                (bus("st"), "out1"),
                (input("a"), "tr"),
                (input("b"), "out1"),
                (bus("out2"), "out1"),
            ]
            .into_iter()
            .map(|(src, dst)| {
                (
                    SendId {
                        src,
                        dst: BusId::new(dst),
                    },
                    Tap::Pre,
                )
            })
            .collect(),
            engineer: None,
        };
        let mut want = MixState::default();
        want.inputs.insert(
            InputId::new("a"),
            InputState {
                trim_db: 7.860745325569493,
                muted: true,
                processing: false,
                fader_db: -2.9863661282611904,
                pan: 0.994263885156115,
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
                trim_db: 6.090730311470619,
                muted: false,
                processing: true,
                fader_db: 6.761666797859263,
                pan: -0.012911063942519174,
                eq: golden_eq(
                    -4.0,
                    [
                        (
                            false,
                            6470.401958192328,
                            0.5021446813616546,
                            0.19688359597780206,
                        ),
                        (
                            false,
                            11901.148979447818,
                            11.133731280551594,
                            0.16030331371989817,
                        ),
                        (false, 4053.644459677573, -150.0, 0.7825190766565605),
                        (
                            true,
                            735.5941438025018,
                            -11.470716544718346,
                            2.5092624167790056,
                        ),
                        (
                            false,
                            1200.6210660070149,
                            -4.96324158170968,
                            2.666851871684069,
                        ),
                    ],
                ),
            },
        );
        want.inputs.insert(
            InputId::new("c"),
            InputState {
                trim_db: 9.74874610834748,
                muted: false,
                processing: true,
                fader_db: -1.8094168493480733,
                pan: -0.18455953606452713,
                eq: golden_eq(
                    -5.0,
                    [
                        (
                            true,
                            3882.815203156875,
                            -8.386903782575477,
                            1.5795244724403958,
                        ),
                        (
                            true,
                            1924.0233518485884,
                            -11.01217083172865,
                            0.18260427270756532,
                        ),
                        (true, 4492.025189474921, -150.0, 2.7735297299149155),
                        (
                            true,
                            10912.60490330623,
                            -7.865116961622644,
                            1.93032652129178,
                        ),
                        (
                            false,
                            5026.636099729572,
                            -3.307348655680954,
                            1.228379515053895,
                        ),
                    ],
                ),
            },
        );
        want.buses.insert(
            BusId::new("out1"),
            BusState {
                fader_db: 2.495521336750077,
                pan: -0.46934761714109086,
                muted: true,
                eq: golden_eq(
                    0.0,
                    [
                        (
                            false,
                            9742.087017799977,
                            -6.0805269907599255,
                            1.8584697541449708,
                        ),
                        (
                            true,
                            4851.60332530278,
                            8.858507609198632,
                            2.0176527131274087,
                        ),
                        (false, 10854.533206212067, -150.0, 0.15009490173878062),
                        (
                            true,
                            7073.5491137812,
                            -9.841863137281184,
                            0.9573047008724025,
                        ),
                        (
                            true,
                            8759.01356258902,
                            -0.13073934463131387,
                            2.5668502745471504,
                        ),
                    ],
                ),
                limiter: Limiter {
                    enabled: false,
                    limit_db: -2.898392646264181,
                },
            },
        );
        want.buses.insert(
            BusId::new("out2"),
            BusState {
                fader_db: -8.872329496139283,
                pan: -0.9938020829404317,
                muted: false,
                eq: golden_eq(
                    0.0,
                    [
                        (
                            false,
                            8552.733893251341,
                            -3.6854257470868887,
                            1.1572525961080844,
                        ),
                        (
                            true,
                            10293.178110885969,
                            11.769178998214443,
                            1.4402821252744298,
                        ),
                        (true, 362.99649068234925, -150.0, 2.663431534906306),
                        (
                            false,
                            4146.844681567507,
                            -4.374104574700849,
                            1.1871577437579943,
                        ),
                        (
                            true,
                            4591.231351786333,
                            2.0732156837345244,
                            2.008289927755123,
                        ),
                    ],
                ),
                limiter: Limiter {
                    enabled: true,
                    limit_db: -4.4514733044767265,
                },
            },
        );
        want.buses.insert(
            BusId::new("st"),
            BusState {
                fader_db: 2.1840260959726443,
                pan: -0.3075480587313071,
                muted: false,
                eq: golden_eq(
                    -4.0,
                    [
                        (
                            true,
                            765.3514794650102,
                            -10.692908852337979,
                            2.8197269010741532,
                        ),
                        (
                            false,
                            10258.236684863237,
                            10.241366306564963,
                            0.6297656910649826,
                        ),
                        (false, 5407.292950244168, -150.0, 0.6789130628655704),
                        (
                            true,
                            5760.929344855779,
                            11.159369543006008,
                            1.5455577584036342,
                        ),
                        (
                            true,
                            8446.224609434617,
                            -8.036087287485376,
                            1.4131039713976368,
                        ),
                    ],
                ),
                ..BusState::default()
            },
        );
        want.buses.insert(
            BusId::new("tr"),
            BusState {
                fader_db: -11.952896559775493,
                pan: 0.09452584976670364,
                muted: false,
                ..BusState::default()
            },
        );
        want.buses.insert(
            BusId::new("master"),
            BusState {
                fader_db: -9.208932751919967,
                pan: 0.29818602483110324,
                muted: false,
                ..BusState::default()
            },
        );
        want.sends.push(SendEntry {
            id: SendId {
                src: Source::Input(InputId::new("a")),
                dst: BusId::new("out1"),
            },
            state: SendState {
                gain_db: -150.0,
                pan: 0.15941987738312036,
                muted: false,
            },
        });
        want.sends.push(SendEntry {
            id: SendId {
                src: Source::Input(InputId::new("a")),
                dst: BusId::new("tr"),
            },
            state: SendState {
                gain_db: -150.0,
                pan: 0.1503492464787195,
                muted: false,
            },
        });
        want.sends.push(SendEntry {
            id: SendId {
                src: Source::Input(InputId::new("b")),
                dst: BusId::new("out1"),
            },
            state: SendState {
                gain_db: -35.566241320880216,
                pan: 0.6731766466410807,
                muted: false,
            },
        });
        want.sends.push(SendEntry {
            id: SendId {
                src: Source::Input(InputId::new("b")),
                dst: BusId::new("out2"),
            },
            state: SendState {
                gain_db: -32.14479707284505,
                pan: -0.3452460250054328,
                muted: false,
            },
        });
        want.sends.push(SendEntry {
            id: SendId {
                src: Source::Input(InputId::new("c")),
                dst: BusId::new("st"),
            },
            state: SendState {
                gain_db: -47.55859137801734,
                pan: 0.8308216760923159,
                muted: true,
            },
        });
        want.sends.push(SendEntry {
            id: SendId {
                src: Source::Bus(BusId::new("out2")),
                dst: BusId::new("out1"),
            },
            state: SendState {
                gain_db: 7.882211183128575,
                pan: 0.5170551393454226,
                muted: false,
            },
        });
        want.sends.push(SendEntry {
            id: SendId {
                src: Source::Bus(BusId::new("st")),
                dst: BusId::new("out1"),
            },
            state: SendState {
                gain_db: -47.03979025427532,
                pan: 0.8989565683413256,
                muted: false,
            },
        });

        assert_eq!(sample_state(&topo, 5), want);
    }
}
