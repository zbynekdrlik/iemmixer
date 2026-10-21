//! The caps and the shaping of a state by a topology.

use super::*;
use crate::msg::{GroupInfo, InputInfo, MixInfo};

fn input(id: &str) -> InputId {
    InputId::new(id)
}

fn mix(id: &str) -> MixId {
    MixId::new(id)
}

fn level(gain_db: f64, pan: f64) -> Level {
    Level {
        gain_db,
        pan,
        ..Level::default()
    }
}

/// Inputs `a`, `b` (in group `g`); mixes `s1` (hears `s2` as a peer),
/// `s2` (hears nothing) and `eng` (hears `s1` and `s2` whole).
fn topology() -> TopologyInfo {
    let info = |id: &str, mixes: &[&str], peers: &[&str]| MixInfo {
        id: mix(id),
        channels: 2,
        mixes: mixes.iter().map(|m| mix(m)).collect(),
        peers: peers.iter().map(|m| mix(m)).collect(),
    };
    TopologyInfo {
        hash: "h".into(),
        sample_rate: 96_000,
        engineer: mix("eng"),
        inputs: vec![
            InputInfo {
                id: input("a"),
                channels: 1,
                talkback: false,
                group: None,
            },
            InputInfo {
                id: input("b"),
                channels: 1,
                talkback: false,
                group: Some(GroupId::new("g")),
            },
        ],
        groups: vec![GroupInfo {
            id: GroupId::new("g"),
            inputs: vec![input("b")],
        }],
        mixes: vec![
            info("s1", &[], &["s2"]),
            info("s2", &[], &[]),
            info("eng", &["s1", "s2"], &[]),
        ],
    }
}

// Moved from the engine's `params` with the caps (#58 re-review).
#[test]
fn cap_clamps_and_rejects_non_finite() {
    assert_eq!(cap(40.0, FADER_DB), Some(12.0));
    assert_eq!(cap(-1e308, FADER_DB), Some(DB_OFF));
    assert_eq!(cap(3.0, PAN), Some(1.0));
    assert_eq!(cap(-0.25, PAN), Some(-0.25));
    assert_eq!(cap(f64::NAN, PAN), None);
    assert_eq!(cap(f64::INFINITY, PAN), None);
    assert_eq!(EQ_GAIN_DB.1, 20.0 * 4f64.log10());
}

#[test]
fn state_caps_replace_non_finite_with_defaults() {
    let mut eq = Eq {
        gain_db: f64::NAN,
        ..Eq::default()
    };
    eq.bands[1].freq_hz = 1e9;
    eq.bands[2].gain_db = f64::NEG_INFINITY;
    eq.bands[3].bw_oct = 0.0;
    let c = cap_eq(&eq);
    assert_eq!(c.gain_db, 0.0);
    assert_eq!(c.bands[1].freq_hz, 24_000.0);
    assert_eq!(c.bands[2].gain_db, 0.0);
    assert_eq!(c.bands[3].bw_oct, 0.01);
    assert_eq!(c.bands[0], eq.bands[0]);
    assert!(!eq_is_finite(&eq));
    assert!(eq_is_finite(&c));
    let cases: [fn(&mut Eq); 3] = [
        |e| e.bands[0].freq_hz = f64::NAN,
        |e| e.bands[4].bw_oct = f64::INFINITY,
        |e| e.gain_db = f64::INFINITY,
    ];
    for bad in cases {
        let mut e = Eq::default();
        bad(&mut e);
        assert!(!eq_is_finite(&e));
    }
    let i = cap_input(&InputState {
        trim_db: 99.0,
        muted: true,
        processing: false,
        eq,
    });
    assert_eq!((i.trim_db, i.muted, i.processing), (24.0, true, false));
    assert_eq!(i.eq, c);
    assert_eq!(
        cap_input(&InputState {
            trim_db: f64::NAN,
            ..InputState::default()
        })
        .trim_db,
        0.0
    );
    let l = cap_level(&Level {
        gain_db: f64::NAN,
        pan: 2.0,
        muted: true,
    });
    assert_eq!((l.gain_db, l.pan, l.muted), (DB_OFF, 1.0, true));
    let l = cap_level(&Level {
        gain_db: 40.0,
        pan: f64::NAN,
        muted: false,
    });
    assert_eq!((l.gain_db, l.pan), (12.0, 0.0));
    assert_eq!(
        cap_level(&Level {
            gain_db: -3.0,
            ..Level::default()
        })
        .gain_db,
        -3.0
    );
    let g = cap_group(&MixGroup {
        gain_db: f64::INFINITY,
        muted: true,
        eq,
    });
    assert_eq!((g.gain_db, g.muted), (0.0, true));
    assert_eq!(g.eq, c);
    assert_eq!(
        cap_group(&MixGroup {
            gain_db: -1e308,
            ..MixGroup::default()
        })
        .gain_db,
        DB_OFF
    );
    let mut o = MixOut {
        volume_db: 13.0,
        muted: true,
        eq,
        ..MixOut::default()
    };
    o.limiter.limit_db = -9.0;
    let co = cap_out(&o);
    assert_eq!(
        (co.volume_db, co.muted, co.limiter.limit_db),
        (12.0, true, -6.0)
    );
    assert_eq!(co.eq, c);
    o.volume_db = f64::NAN;
    o.limiter.limit_db = f64::NAN;
    assert_eq!(
        (cap_out(&o).volume_db, cap_out(&o).limiter.limit_db),
        (0.0, -6.0)
    );
    o.limiter.limit_db = 3.0;
    o.limiter.enabled = false;
    assert_eq!(cap_out(&o).limiter.limit_db, 0.0);
    assert!(!cap_out(&o).limiter.enabled);
}

#[test]
fn an_id_is_clipped_to_64_bytes_on_a_char_boundary() {
    assert_eq!(clip("short"), "short");
    let long = "x".repeat(70);
    assert_eq!(clip(&long).len(), 64);
    // 63 ASCII bytes, then a two-byte character across byte 64.
    let wide = format!("{}é", "x".repeat(63));
    assert_eq!(clip(&wide), "x".repeat(63));
}

#[test]
fn a_state_is_shaped_by_the_topology_and_what_it_lacks_is_named() {
    let topo = topology();
    let mut state = MixState::default();
    state.inputs.insert(
        input("a"),
        InputState {
            trim_db: 30.0,
            ..InputState::default()
        },
    );
    state.inputs.insert(input("ghost"), InputState::default());
    let mut s1 = Mix::default();
    s1.out.volume_db = 20.0;
    s1.inputs.insert(input("a"), level(-4.0, 0.5));
    s1.inputs.insert(input("ghost"), Level::default());
    s1.groups.insert(GroupId::new("ghost"), MixGroup::default());
    s1.mixes.insert(mix("s2"), level(-6.0, 0.0));
    s1.mixes.insert(mix("eng"), level(0.0, 0.0));
    state.mixes.insert(mix("s1"), s1);
    // s2 took a copy of a set that hears a peer: s2 hears nothing.
    let mut s2 = Mix::default();
    s2.mixes.insert(mix("s1"), level(-3.0, 0.0));
    state.mixes.insert(mix("s2"), s2);
    state.mixes.insert(mix("ghost"), Mix::default());

    let (shaped, dropped) = reconcile(&topo, &state);
    assert_eq!(
        dropped,
        vec![
            "input ghost",
            "mix s1 input ghost",
            "mix s1 group ghost",
            "mix s1 hearing eng",
            "mix s2 hearing s1",
            "mix ghost",
        ]
    );
    assert_eq!(shaped.inputs.len(), 2);
    assert_eq!(shaped.inputs[&input("a")].trim_db, 24.0);
    assert_eq!(shaped.inputs[&input("b")], InputState::default());
    assert_eq!(shaped.mixes.len(), 3);
    let s1 = &shaped.mixes[&mix("s1")];
    assert_eq!(s1.out.volume_db, 12.0);
    assert_eq!(s1.inputs[&input("a")], level(-4.0, 0.5));
    assert_eq!(s1.inputs[&input("b")], Level::default());
    assert_eq!(
        s1.mixes.keys().collect::<Vec<_>>(),
        [&mix("s2")],
        "its peer only"
    );
    assert_eq!(s1.mixes[&mix("s2")], level(-6.0, 0.0));
    assert_eq!(s1.groups.keys().collect::<Vec<_>>(), [&GroupId::new("g")]);
    assert!(shaped.mixes[&mix("s2")].mixes.is_empty(), "hears nothing");
    let eng = &shaped.mixes[&mix("eng")];
    assert_eq!(eng.out, MixOut::default());
    assert_eq!(eng.inputs.len(), 2);
    assert_eq!(eng.mixes.len(), 2, "both, whole, defaulted");
    // A shaped state stays as it is.
    assert_eq!(reconcile(&topo, &shaped), (shaped.clone(), Vec::new()));
}
