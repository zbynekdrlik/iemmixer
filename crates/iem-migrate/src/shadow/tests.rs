//! The shadow's comparison on hand-built synthetic topologies and states,
//! and on `config/test-site.toml` (P6: placeholder ids, synthetic channels).
//! Each difference is checked by kind, id and field, and counted against
//! `iem_rpp`'s own comparisons (`Topology::diff`, `import::compare`), which
//! print the same differences with their values.

use std::path::PathBuf;

use iem_engine::persist::{Persisted, Store};
use iem_engine_proto::{
    BandKind, GroupId, InputId, InputState, Level, Mix, MixGroup, MixOut, Source,
};
use iem_rpp::import::compare;
use iem_rpp::sitegen::{sample_state, synthetic_routing};
use iem_rpp::topology::{TopoGroup, TopoInput, TopoMix};

use super::*;
use crate::site;

fn d(kind: &'static str, id: &str, field: &str) -> Diff {
    Diff {
        kind,
        id: id.to_owned(),
        field: field.to_owned(),
    }
}

fn input(id: &str, rx: &[u16]) -> TopoInput {
    TopoInput {
        id: InputId::new(id),
        rx: rx.to_vec(),
        talkback: false,
    }
}

fn group(id: &str, inputs: &[&str]) -> TopoGroup {
    TopoGroup {
        id: GroupId::new(id),
        inputs: inputs.iter().map(|i| InputId::new(*i)).collect(),
    }
}

fn mix(id: &str, tx: &[u16], hears: &[&str]) -> TopoMix {
    TopoMix {
        id: MixId::new(id),
        tx: tx.to_vec(),
        mixes: hears.iter().map(|h| MixId::new(*h)).collect(),
    }
}

/// Three inputs, one group of two, three stereo mixes (member1 hears the
/// other two) and a mono one.
fn topo() -> Topology {
    Topology {
        inputs: vec![
            input("mic1", &[101]),
            input("mic2", &[102]),
            input("mic3", &[103]),
        ],
        groups: vec![group("stems", &["mic2", "mic3"])],
        mixes: vec![
            mix("member1", &[71, 72], &["member2", "member3"]),
            mix("member2", &[73, 74], &[]),
            mix("member3", &[75, 76], &[]),
            mix("translator", &[93], &[]),
        ],
        engineer: Some(MixId::new("member2")),
    }
}

/// What `Topology::diff` prints before a difference's values.
fn printed_site(d: &Diff) -> String {
    match (d.kind, d.field.as_str()) {
        ("engineer", _) => format!("engineer: {} in the project", d.id),
        (kind, ONLY_PROJECT) => format!("{kind} {}: in the project, not in site.toml", d.id),
        (kind, ONLY_SITE) => format!("{kind} {}: in site.toml, not in the project", d.id),
        (kind, field) => format!("{kind} {}: {field} ", d.id),
    }
}

/// What `import::compare` prints before a difference's values.
fn printed_state(d: &Diff) -> String {
    let (mix, of) = d.id.split_once('/').unwrap_or((d.id.as_str(), ""));
    let who = match d.kind {
        "input" | "mix" => format!("{} {}", d.kind, d.id),
        "level" | "mix_level" => format!("mix {mix} level {of}"),
        "group" => format!("mix {mix} group {of}"),
        other => panic!("kind {other}"),
    };
    let field = d.field.as_str();
    if field == ONLY_IMPORT || field == ONLY_LIVE {
        return format!("{who}: in one state only");
    }
    if let Some(rest) = field.strip_prefix("eq.band") {
        let (n, what) = rest.split_once('.').unwrap();
        let what = if what == "hz" { "Hz" } else { what };
        return format!("{who} band {n}: {what} ");
    }
    let what = if field == "eq.gain" { "EQ gain" } else { field };
    format!("{who}: {what} ")
}

/// Each named difference is the one `iem_rpp` prints at the same place.
fn same_as_printed(named: &[Diff], printed: &[String], as_printed: fn(&Diff) -> String) {
    assert_eq!(named.len(), printed.len(), "{named:#?}\n{printed:#?}");
    for (d, line) in named.iter().zip(printed) {
        assert!(line.starts_with(&as_printed(d)), "{d:?} vs {line:?}");
    }
}

fn changed(f: impl FnOnce(&mut Topology)) -> Topology {
    let mut t = topo();
    f(&mut t);
    t
}

#[test]
fn an_equal_topology_has_no_difference_in_any_order() {
    let site = changed(|t| {
        t.inputs.reverse();
        t.mixes.reverse();
        if let Some(g) = t.groups.first_mut() {
            g.inputs.reverse();
        }
        if let Some(m) = t.mixes.iter_mut().find(|m| m.id == MixId::new("member1")) {
            m.mixes.reverse();
        }
    });
    assert_eq!(site_diffs(&topo(), &site), Vec::<Diff>::new());
    assert_eq!(topo().diff(&site), Vec::<String>::new());
}

#[test]
fn every_topology_difference_is_named_by_kind_id_and_field() {
    type Case = (Box<dyn Fn(&mut Topology)>, Vec<Diff>);
    let cases: Vec<Case> = vec![
        (
            Box::new(|t: &mut Topology| t.inputs[0].rx = vec![104]),
            vec![d("input", "mic1", "rx")],
        ),
        (
            Box::new(|t: &mut Topology| t.inputs[0].talkback = true),
            vec![d("input", "mic1", "talkback")],
        ),
        (
            Box::new(|t: &mut Topology| {
                t.inputs[0].rx = vec![104];
                t.inputs[0].talkback = true;
            }),
            vec![d("input", "mic1", "rx"), d("input", "mic1", "talkback")],
        ),
        (
            Box::new(|t: &mut Topology| {
                t.inputs.remove(1);
            }),
            vec![d("input", "mic2", ONLY_PROJECT)],
        ),
        (
            Box::new(|t: &mut Topology| t.inputs.push(input("mic4", &[104]))),
            vec![d("input", "mic4", ONLY_SITE)],
        ),
        (
            Box::new(|t: &mut Topology| t.groups[0].inputs = vec![InputId::new("mic2")]),
            vec![d("group", "stems", "inputs")],
        ),
        (
            Box::new(|t: &mut Topology| t.groups.clear()),
            vec![d("group", "stems", ONLY_PROJECT)],
        ),
        (
            Box::new(|t: &mut Topology| t.groups.push(group("keys", &["mic1"]))),
            vec![d("group", "keys", ONLY_SITE)],
        ),
        (
            Box::new(|t: &mut Topology| t.mixes[0].tx = vec![77, 78]),
            vec![d("mix", "member1", "tx")],
        ),
        (
            Box::new(|t: &mut Topology| t.mixes[0].mixes = vec![MixId::new("member2")]),
            vec![d("mix", "member1", "hears")],
        ),
        (
            Box::new(|t: &mut Topology| {
                t.mixes[0].tx = vec![77, 78];
                t.mixes[0].mixes.clear();
            }),
            vec![d("mix", "member1", "tx"), d("mix", "member1", "hears")],
        ),
        (
            Box::new(|t: &mut Topology| {
                t.mixes.pop();
            }),
            vec![d("mix", "translator", ONLY_PROJECT)],
        ),
        (
            Box::new(|t: &mut Topology| t.mixes.push(mix("member4", &[77, 78], &[]))),
            vec![d("mix", "member4", ONLY_SITE)],
        ),
        (
            Box::new(|t: &mut Topology| t.engineer = Some(MixId::new("member1"))),
            vec![d("engineer", "member2", "mix")],
        ),
        (
            Box::new(|t: &mut Topology| t.engineer = None),
            vec![d("engineer", "member2", "mix")],
        ),
        // Every kind at once, in `Topology::diff`'s order.
        (
            Box::new(|t: &mut Topology| {
                t.inputs[2].rx = vec![105];
                t.groups.push(group("keys", &["mic1"]));
                t.mixes[1].tx = vec![79, 80];
                t.engineer = None;
            }),
            vec![
                d("input", "mic3", "rx"),
                d("group", "keys", ONLY_SITE),
                d("mix", "member2", "tx"),
                d("engineer", "member2", "mix"),
            ],
        ),
    ];
    for (i, (change, want)) in cases.iter().enumerate() {
        let site = changed(|t| change(t));
        let got = site_diffs(&topo(), &site);
        assert_eq!(&got, want, "case {i}");
        same_as_printed(&got, &topo().diff(&site), printed_site);
    }
    // A project without an engineer names `none`.
    let project = changed(|t| t.engineer = None);
    assert_eq!(
        site_diffs(&project, &topo()),
        [d("engineer", "none", "mix")]
    );
}

fn level(gain_db: f64) -> Level {
    Level {
        gain_db,
        pan: 0.0,
        muted: false,
    }
}

/// What the project holds: every input level and group strip of the
/// stereo mixes, their heard mixes, and the translator's mic1 only; never
/// member2's level of mic3.
fn routing(t: &Topology) -> Routing {
    let mut r = Routing::default();
    for m in &t.mixes {
        if m.tx.len() != 2 {
            r.levels
                .insert((m.id.clone(), Source::Input(InputId::new("mic1"))));
            continue;
        }
        for i in &t.inputs {
            if !(m.id == MixId::new("member2") && i.id == InputId::new("mic3")) {
                r.levels.insert((m.id.clone(), Source::Input(i.id.clone())));
            }
        }
        for h in &m.mixes {
            r.levels.insert((m.id.clone(), Source::Mix(h.clone())));
        }
        for g in &t.groups {
            r.strips.insert((m.id.clone(), g.id.clone()));
        }
    }
    r
}

/// A state with a value at every place a mix can hold one.
fn state(t: &Topology) -> MixState {
    let mut s = MixState::default();
    for i in &t.inputs {
        s.inputs.insert(i.id.clone(), InputState::default());
    }
    for m in &t.mixes {
        let x = Mix {
            out: MixOut::default(),
            inputs: t
                .inputs
                .iter()
                .map(|i| (i.id.clone(), level(-6.0)))
                .collect(),
            groups: t
                .groups
                .iter()
                .map(|g| (g.id.clone(), MixGroup::default()))
                .collect(),
            mixes: m.mixes.iter().map(|h| (h.clone(), level(-12.0))).collect(),
        };
        s.mixes.insert(m.id.clone(), x);
    }
    s
}

fn inp<'a>(s: &'a mut MixState, id: &str) -> &'a mut InputState {
    s.inputs.get_mut(&InputId::new(id)).unwrap()
}

fn mx<'a>(s: &'a mut MixState, id: &str) -> &'a mut Mix {
    s.mixes.get_mut(&MixId::new(id)).unwrap()
}

const TOL: f64 = 1e-5;

#[test]
fn equal_states_have_no_difference() {
    let t = topo();
    let s = state(&t);
    assert_eq!(
        state_diffs(&t, &routing(&t), &s, &s, TOL),
        Vec::<Diff>::new()
    );
    // Within the tolerance, and both levels off, is the same.
    let mut near = s.clone();
    inp(&mut near, "mic1").trim_db = 0.5 * TOL;
    mx(&mut near, "member1")
        .inputs
        .insert(InputId::new("mic1"), level(-200.0));
    let mut off = s.clone();
    mx(&mut off, "member1")
        .inputs
        .insert(InputId::new("mic1"), level(-300.0));
    assert_eq!(
        state_diffs(&t, &routing(&t), &near, &off, TOL),
        Vec::<Diff>::new()
    );
}

#[test]
fn every_state_difference_is_named_by_kind_id_and_field() {
    type Case = (Box<dyn Fn(&mut MixState)>, Vec<Diff>);
    let cases: Vec<Case> = vec![
        (
            Box::new(|s: &mut MixState| inp(s, "mic1").trim_db = 3.0),
            vec![d("input", "mic1", "trim")],
        ),
        (
            Box::new(|s: &mut MixState| inp(s, "mic1").muted = true),
            vec![d("input", "mic1", "muted")],
        ),
        (
            Box::new(|s: &mut MixState| inp(s, "mic1").processing = false),
            vec![d("input", "mic1", "processing")],
        ),
        (
            Box::new(|s: &mut MixState| inp(s, "mic1").eq.gain_db = 2.0),
            vec![d("input", "mic1", "eq.gain")],
        ),
        (
            Box::new(|s: &mut MixState| inp(s, "mic1").eq.bands[1].kind = BandKind::HighShelf),
            vec![d("input", "mic1", "eq.band2.kind")],
        ),
        (
            Box::new(|s: &mut MixState| inp(s, "mic1").eq.bands[1].enabled = true),
            vec![d("input", "mic1", "eq.band2.enabled")],
        ),
        (
            Box::new(|s: &mut MixState| inp(s, "mic1").eq.bands[1].freq_hz = 250.0),
            vec![d("input", "mic1", "eq.band2.hz")],
        ),
        (
            Box::new(|s: &mut MixState| inp(s, "mic1").eq.bands[1].gain_db = 4.0),
            vec![d("input", "mic1", "eq.band2.gain")],
        ),
        (
            Box::new(|s: &mut MixState| inp(s, "mic1").eq.bands[1].bw_oct = 0.5),
            vec![d("input", "mic1", "eq.band2.octaves")],
        ),
        (
            Box::new(|s: &mut MixState| inp(s, "mic2").eq.bands[4].gain_db = 4.0),
            vec![d("input", "mic2", "eq.band5.gain")],
        ),
        (
            Box::new(|s: &mut MixState| {
                s.inputs.remove(&InputId::new("mic2"));
            }),
            vec![d("input", "mic2", ONLY_IMPORT)],
        ),
        (
            Box::new(|s: &mut MixState| mx(s, "member1").out.volume_db = -3.0),
            vec![d("mix", "member1", "volume")],
        ),
        (
            Box::new(|s: &mut MixState| mx(s, "member1").out.muted = true),
            vec![d("mix", "member1", "muted")],
        ),
        (
            Box::new(|s: &mut MixState| mx(s, "member1").out.eq.gain_db = 1.0),
            vec![d("mix", "member1", "eq.gain")],
        ),
        (
            Box::new(|s: &mut MixState| mx(s, "member1").out.limiter.enabled = false),
            vec![d("mix", "member1", "limiter")],
        ),
        (
            Box::new(|s: &mut MixState| mx(s, "member1").out.limiter.limit_db = -3.0),
            vec![d("mix", "member1", "limit")],
        ),
        (
            Box::new(|s: &mut MixState| {
                s.mixes.remove(&MixId::new("member3"));
            }),
            vec![d("mix", "member3", ONLY_IMPORT)],
        ),
        (
            Box::new(|s: &mut MixState| {
                mx(s, "member1")
                    .inputs
                    .insert(InputId::new("mic1"), level(-1.0));
            }),
            vec![d("level", "member1/mic1", "gain")],
        ),
        (
            Box::new(|s: &mut MixState| {
                let l = mx(s, "member1").inputs.get_mut(&InputId::new("mic1"));
                l.unwrap().pan = 0.5;
            }),
            vec![d("level", "member1/mic1", "pan")],
        ),
        (
            Box::new(|s: &mut MixState| {
                let l = mx(s, "member1").inputs.get_mut(&InputId::new("mic1"));
                l.unwrap().muted = true;
            }),
            vec![d("level", "member1/mic1", "muted")],
        ),
        (
            Box::new(|s: &mut MixState| {
                mx(s, "member1").inputs.remove(&InputId::new("mic1"));
            }),
            vec![d("level", "member1/mic1", ONLY_IMPORT)],
        ),
        (
            Box::new(|s: &mut MixState| {
                mx(s, "member1")
                    .mixes
                    .insert(MixId::new("member2"), level(-1.0));
            }),
            vec![d("mix_level", "member1/member2", "gain")],
        ),
        (
            Box::new(|s: &mut MixState| {
                mx(s, "member1").mixes.remove(&MixId::new("member3"));
            }),
            vec![d("mix_level", "member1/member3", ONLY_IMPORT)],
        ),
        (
            Box::new(|s: &mut MixState| {
                let g = mx(s, "member1").groups.get_mut(&GroupId::new("stems"));
                g.unwrap().gain_db = -2.0;
            }),
            vec![d("group", "member1/stems", "gain")],
        ),
        (
            Box::new(|s: &mut MixState| {
                let g = mx(s, "member1").groups.get_mut(&GroupId::new("stems"));
                g.unwrap().muted = true;
            }),
            vec![d("group", "member1/stems", "muted")],
        ),
        (
            Box::new(|s: &mut MixState| {
                let g = mx(s, "member1").groups.get_mut(&GroupId::new("stems"));
                g.unwrap().eq.bands[0].enabled = true;
            }),
            vec![d("group", "member1/stems", "eq.band1.enabled")],
        ),
        (
            Box::new(|s: &mut MixState| {
                mx(s, "member1").groups.remove(&GroupId::new("stems"));
            }),
            vec![d("group", "member1/stems", ONLY_IMPORT)],
        ),
        // What the project cannot hold is never compared: member2's level
        // of mic3 (no receive) and the mono translator's EQ and limiter.
        (
            Box::new(|s: &mut MixState| {
                mx(s, "member2")
                    .inputs
                    .insert(InputId::new("mic3"), level(-1.0));
                let t = mx(s, "translator");
                t.out.eq.gain_db = 5.0;
                t.out.limiter.enabled = false;
            }),
            vec![],
        ),
    ];
    let t = topo();
    let r = routing(&t);
    for (i, (change, want)) in cases.iter().enumerate() {
        let import = state(&t);
        let mut live = import.clone();
        change(&mut live);
        let got = state_diffs(&t, &r, &import, &live, TOL);
        assert_eq!(&got, want, "case {i}");
        same_as_printed(&got, &compare(&t, &r, &import, &live, TOL), printed_state);
        // The other way round, an id on one side only is the live state's.
        let back = state_diffs(&t, &r, &live, &import, TOL);
        let flipped: Vec<Diff> = want
            .iter()
            .map(|x| match x.field.as_str() {
                ONLY_IMPORT => d(x.kind, &x.id, ONLY_LIVE),
                _ => x.clone(),
            })
            .collect();
        assert_eq!(back, flipped, "case {i} reversed");
    }
}

#[test]
fn an_import_refuses_on_its_topology_first_then_on_what_does_not_fit() {
    let one = [d("input", "mic1", "rx")];
    assert_eq!(verdict(&[], 0, 0), "writes");
    assert_eq!(verdict(&[], 1, 0), "refuses_fit");
    assert_eq!(verdict(&[], 1, 1), "refuses_fit");
    assert_eq!(verdict(&[], 0, 1), "refuses_doubts");
    assert_eq!(verdict(&one, 0, 0), "refuses_topology");
    assert_eq!(verdict(&one, 2, 1), "refuses_topology");
}

#[test]
fn the_saved_state_is_named_by_the_file_it_came_from() {
    assert_eq!(state_from(Saved::Current), Some("current"));
    assert_eq!(state_from(Saved::Interrupted), Some("interrupted"));
    assert_eq!(state_from(Saved::Generation(3)), Some("generation"));
    assert_eq!(state_from(Saved::Baseline), Some("baseline"));
    assert_eq!(state_from(Saved::Defaults), None);
}

#[test]
fn the_report_is_one_json_object_of_counts_and_named_differences() {
    let r = Report {
        counts: Counts {
            tracks: 45,
            sends: 268,
            eqs: 44,
            limiters: 10,
            trims: 24,
        },
        site: vec![d("mix", "member1", "tx")],
        fit: 2,
        state_from: "current",
        state: vec![d("level", "member1/mic1", "gain")],
        doubts: 1,
    };
    assert_eq!(
        r.json(),
        json!({
            "import": "refuses_topology",
            "counts": {"tracks": 45, "sends": 268, "eqs": 44, "limiters": 10, "trims": 24},
            "site": [{"kind": "mix", "id": "member1", "field": "tx"}],
            "fit": 2,
            "state_from": "current",
            "state": [{"kind": "level", "id": "member1/mic1", "field": "gain"}],
            "doubts": 1,
        })
    );
    assert_eq!(
        unmappable(3),
        json!({"import": "unmappable", "problems": 3})
    );
}

fn test_site() -> SiteFile {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config/test-site.toml");
    site::open(&path).unwrap()
}

/// The import of a project that holds exactly `state` on the test site.
fn imported(sf: &SiteFile, state: MixState) -> Imported {
    let topology = sf.topology.clone();
    let routing = synthetic_routing(&topology);
    Imported {
        topology,
        routing,
        state,
        counts: Counts::default(),
        notes: Vec::new(),
        tracks: Vec::new(),
    }
}

/// `live` saved through the engine's store into a new directory, and loaded.
fn saved(sf: &SiteFile, live: &MixState) -> (tempfile::TempDir, Loaded) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store
        .save(&Persisted {
            topology_hash: sf.compiled.hash.clone(),
            state: live.clone(),
            ..Persisted::default()
        })
        .unwrap();
    let loaded = store.load(&sf.compiled);
    (dir, loaded)
}

#[test]
fn a_project_equal_to_the_saved_state_is_clean() {
    let sf = test_site();
    let routing = synthetic_routing(&sf.topology);
    let state = sample_state(&sf.topology, &routing, 4);
    let (_dir, loaded) = saved(&sf, &state);
    let mut imp = imported(&sf, state);
    imp.counts.tracks = 45;
    let r = shadow(&imp, &sf, Some(&loaded));
    assert_eq!(
        (r.site.len(), r.fit, r.state_from, r.state.len(), r.doubts),
        (0, 0, "current", 0, 0),
        "{r:?}"
    );
    assert_eq!(r.counts.tracks, 45);
    assert_eq!(verdict(&r.site, r.fit, r.doubts), "writes");
}

#[test]
fn the_state_an_import_would_write_is_compared_with_the_saved_one() {
    let sf = test_site();
    let routing = synthetic_routing(&sf.topology);
    let project = sample_state(&sf.topology, &routing, 4);
    let live = sample_state(&sf.topology, &routing, 9);
    let (_dir, mut loaded) = saved(&sf, &live);
    loaded.doubts.push("a doubt".to_owned());
    let r = shadow(&imported(&sf, project.clone()), &sf, Some(&loaded));
    let would = to_state(&sf.compiled, &reconcile(&sf.compiled, &project).0);
    let printed = compare(
        &sf.topology,
        &routing,
        &would,
        &loaded.persisted.state,
        CAP_TOLERANCE_DB,
    );
    assert!(r.state.len() > 100, "{}", r.state.len());
    same_as_printed(&r.state, &printed, printed_state);
    assert_eq!((r.state_from, r.doubts, r.fit), ("current", 1, 0));
    // Nothing saved: nothing compared, whatever the directory holds.
    let empty = tempfile::tempdir().unwrap();
    let defaults = Store::open(empty.path()).unwrap().load(&sf.compiled);
    let r = shadow(&imported(&sf, project.clone()), &sf, Some(&defaults));
    assert_eq!((r.state_from, r.state.len()), ("none", 0));
    let r = shadow(&imported(&sf, project), &sf, None);
    assert_eq!((r.state_from, r.state.len(), r.doubts), ("none", 0, 0));
}

#[test]
fn values_the_engine_would_drop_or_cap_are_counted_and_refused() {
    let sf = test_site();
    let routing = synthetic_routing(&sf.topology);
    let mut project = sample_state(&sf.topology, &routing, 4);
    // One input site.toml does not have (dropped), two trims over the cap.
    project
        .inputs
        .insert(InputId::new("mic99"), InputState::default());
    let ids: Vec<InputId> = sf
        .topology
        .inputs
        .iter()
        .take(2)
        .map(|i| i.id.clone())
        .collect();
    for id in &ids {
        project.inputs.get_mut(id).unwrap().trim_db = 1000.0;
    }
    let r = shadow(&imported(&sf, project), &sf, None);
    assert_eq!(r.fit, 3, "{r:?}");
    assert_eq!(verdict(&r.site, r.fit, r.doubts), "refuses_fit");
    // A project whose topology differs refuses on it.
    let mut imp = imported(&sf, MixState::default());
    imp.topology.inputs.remove(0);
    let r = shadow(&imp, &sf, None);
    assert_eq!(r.site.len(), 1, "{r:?}");
    assert_eq!(r.site[0].field, ONLY_SITE);
}
