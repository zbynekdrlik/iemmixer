//! Imports, the model's stages and strips, read-only commands, the muted
//! defaults, the full sync and the state's round trip through the topology.

use super::*;
use iem_engine_proto::{Limiter, Mix};

#[test]
fn import_replaces_state_and_fits_one_block() {
    let mut c = core(Flags::default());
    c.apply(&set_mix("member1", Some(-9.0), None)).unwrap();
    let mut state = MixState::default();
    let mut m2 = Mix::default();
    m2.out.volume_db = 50.0;
    m2.inputs.insert(
        input("mic1"),
        Level {
            gain_db: -4.0,
            ..Level::default()
        },
    );
    m2.inputs.insert(input("ghost"), Level::default());
    m2.groups.insert(GroupId::new("ghost"), MixGroup::default());
    m2.mixes.insert(mix("member3"), Level::default());
    state.mixes.insert(mix("member2"), m2);
    state.mixes.insert(mix("ghost"), Mix::default());
    state.inputs.insert(input("ghost"), InputState::default());
    let (r, dropped) = reconcile(c.topology(), &state);
    assert_eq!(
        dropped,
        vec![
            "input ghost",
            "mix member2 input ghost",
            "mix member2 group ghost",
            "mix member2 hearing member3",
            "mix ghost"
        ]
    );
    assert_eq!(r.mixes.len(), 11);
    assert_eq!(
        r.mixes.iter().map(|x| x.levels.len()).sum::<usize>(),
        11 * 24 + 17
    );
    let out = c
        .apply(&Cmd::ImportState {
            state,
            baseline: true,
        })
        .unwrap();
    assert_eq!(out.rev, 2);
    assert_eq!(out.effect, Effect::Imported { baseline: true });
    assert!(out.rt.len() <= MAX_CMDS_PER_BLOCK);
    assert_eq!(out.rt.len(), 24 * 2 + 11 * 3 + 11 * 24 + 17 + 11 * 2);
    let st = c.state();
    assert_eq!(st.mixes[&mix("member2")].out.volume_db, 12.0);
    assert_eq!(
        st.mixes[&mix("member2")].inputs[&input("mic1")].gain_db,
        -4.0
    );
    assert_eq!(st.mixes[&mix("member1")].out.volume_db, 0.0);
    assert!(!st.mixes.contains_key(&mix("ghost")));
    assert_eq!(st.mixes.len(), 11);
    assert!(st.mixes.values().all(|m| m.inputs.len() == 24));
    assert_eq!(st.mixes[&mix("member1")].mixes.len(), 8);
    assert_eq!(st.mixes[&mix("engineer")].mixes.len(), 9);
    assert!(st.mixes.values().all(|m| m.groups.len() == 1));
    // A core rebuilt from the state carries the same state and revision.
    let again = Core::new(Arc::clone(c.topology()), &st, c.rev(), Flags::default());
    assert_eq!(again.state(), st);
    assert_eq!(again.rev(), 2);
    assert_eq!(again.full_sync(), c.full_sync());
}

#[test]
fn every_mix_has_an_eq_a_limiter_and_group_strips() {
    let mut c = core(Flags::default());
    let t = Arc::clone(c.topology());
    let tr = t.mix_index(&mix("translator")).unwrap() as u16;
    let mut eq = Eq::default();
    eq.bands[0].enabled = true;
    let out = c
        .apply(&Cmd::SetEq {
            target: EqTarget::Mix(mix("translator")),
            eq,
        })
        .unwrap();
    assert!(matches!(out.rt.as_slice(), [RtOp::MixEq { m, .. }] if *m == tr));
    let out = c
        .apply(&Cmd::SetEq {
            target: EqTarget::Group {
                mix: mix("member1"),
                group: stems(),
            },
            eq,
        })
        .unwrap();
    assert!(matches!(out.rt.as_slice(), [RtOp::GroupEq { g: 0, .. }]));
    assert_eq!(c.state().mixes[&mix("member1")].groups[&stems()].eq, eq);
    let out = c
        .apply(&Cmd::SetLimiter {
            mix: mix("engineer"),
            enabled: Some(false),
            limit_db: Some(-9.0),
        })
        .unwrap();
    let m = t.engineer as u16;
    assert_eq!(
        out.rt,
        vec![RtOp::Limiter {
            m,
            enabled: false,
            limit_db: -6.0
        }]
    );
    assert_eq!(
        c.state().mixes[&mix("engineer")].out.limiter,
        Limiter {
            enabled: false,
            limit_db: -6.0
        }
    );
    let reset = c
        .apply(&Cmd::ResetLimiterStats {
            mix: mix("engineer"),
        })
        .unwrap();
    assert_eq!(reset.rt, vec![RtOp::ResetLimiter { m }]);
    assert_eq!(
        reset.changes,
        vec![Change::LimiterStatsReset {
            mix: mix("engineer")
        }]
    );
    assert_eq!(reset.rev, 4);
    // A mute on a mix with EQ and limiter emits only the output op.
    let mute = c.apply(&set_mix("engineer", None, Some(true))).unwrap();
    assert!(matches!(
        mute.rt.as_slice(),
        [RtOp::MixOut { muted: true, .. }]
    ));
    // An EQ change on a group strip does not touch its fader.
    let mut other = eq;
    other.bands[1].enabled = true;
    let out = c
        .apply(&Cmd::SetEq {
            target: EqTarget::Group {
                mix: mix("member1"),
                group: stems(),
            },
            eq: other,
        })
        .unwrap();
    assert_eq!(out.rt.len(), 1);
}

#[test]
fn a_group_strip_keeps_what_a_command_leaves_out() {
    let mut c = core(Flags::default());
    let m = c.topology().mix_index(&mix("member1")).unwrap() as u16;
    let strip = |c: &Core| c.state().mixes[&mix("member1")].groups[&stems()];
    c.apply(&Cmd::SetGroup {
        mix: mix("member1"),
        group: stems(),
        gain_db: Some(-10.0),
        muted: Some(true),
    })
    .unwrap();
    // An EQ keeps the fader and the mute…
    let mut eq = Eq::default();
    eq.bands[0].enabled = true;
    let out = c
        .apply(&Cmd::SetEq {
            target: EqTarget::Group {
                mix: mix("member1"),
                group: stems(),
            },
            eq,
        })
        .unwrap();
    assert!(
        matches!(out.rt.as_slice(), [RtOp::GroupEq { g: 0, .. }]),
        "{:?}",
        out.rt
    );
    assert_eq!(
        strip(&c),
        MixGroup {
            gain_db: -10.0,
            muted: true,
            eq
        }
    );
    // …a fader keeps the mute and the EQ.
    let out = c
        .apply(&Cmd::SetGroup {
            mix: mix("member1"),
            group: stems(),
            gain_db: Some(-4.0),
            muted: None,
        })
        .unwrap();
    assert_eq!(
        out.rt,
        vec![RtOp::Group {
            m,
            g: 0,
            gain: 10f64.powf(-4.0 / 20.0),
            muted: true
        }]
    );
    assert_eq!(
        strip(&c),
        MixGroup {
            gain_db: -4.0,
            muted: true,
            eq
        }
    );
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
fn defaults_mute_every_mix_and_leave_levels_off() {
    let t = test_site();
    let d = defaults_muted(&t);
    assert_eq!(d.mixes.len(), 11);
    for m in d.mixes.values() {
        assert!(m.out.muted);
        assert!(m.inputs.values().all(|l| *l == Level::default()));
        assert!(m.groups.values().all(|g| *g == MixGroup::default()));
    }
    assert_eq!(d.inputs.len(), 24);
}

#[test]
fn full_sync_covers_every_stage() {
    let c = core(Flags::default());
    let ops = c.full_sync();
    let count = |f: fn(&RtOp) -> bool| ops.iter().filter(|o| f(o)).count();
    assert_eq!(count(|o| matches!(o, RtOp::Input { .. })), 24);
    assert_eq!(count(|o| matches!(o, RtOp::InputEq { .. })), 24);
    assert_eq!(count(|o| matches!(o, RtOp::MixOut { .. })), 11);
    assert_eq!(count(|o| matches!(o, RtOp::MixEq { .. })), 11);
    assert_eq!(count(|o| matches!(o, RtOp::Limiter { .. })), 11);
    assert_eq!(count(|o| matches!(o, RtOp::Group { .. })), 11);
    assert_eq!(count(|o| matches!(o, RtOp::GroupEq { .. })), 11);
    assert_eq!(
        count(|o| matches!(o, RtOp::Level { gain, .. } if *gain == 0.0)),
        11 * 24 + 17
    );
    let err: ErrorBody = CmdError::new(ErrCode::Forbidden, "x").into();
    assert_eq!(err.code, ErrCode::Forbidden);
}

#[test]
fn state_round_trips_through_the_topology_order() {
    let t = Arc::new(test_site());
    let mut c = Core::new(Arc::clone(&t), &MixState::default(), 0, Flags::default());
    c.apply(&set_level("engineer", Source::Mix(mix("member9")), -2.0))
        .unwrap();
    c.apply(&set_level("translator", src("hand1"), -1.0))
        .unwrap();
    let st = c.state();
    assert_eq!(
        st.mixes[&mix("engineer")].mixes[&mix("member9")].gain_db,
        -2.0
    );
    assert_eq!(
        st.mixes[&mix("translator")].inputs[&input("hand1")].gain_db,
        -1.0
    );
    assert!(st.mixes[&mix("translator")].mixes.is_empty());
    let (r, dropped) = reconcile(&t, &st);
    assert!(dropped.is_empty());
    assert_eq!(to_state(&t, &r), st);
}
