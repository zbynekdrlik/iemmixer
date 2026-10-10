//! The view's tests: domains, channels, updates, commands, EQ and the limiter.

use super::*;
use crate::site_view::tests::test_view;
use iem_engine_proto::{
    EngineMsg, GroupId, Level, Limiter, MixGroup, MixId, MixOut, MixState, Transient,
};

fn page(v: &SiteView, id: &str) -> Page {
    v.page(id).unwrap()
}

fn member(sub: &str) -> Viewer {
    Viewer {
        sub: sub.into(),
        engineer: false,
    }
}

fn engineer() -> Viewer {
    Viewer {
        sub: "engineer".into(),
        engineer: true,
    }
}

fn mirror_with(f: impl FnOnce(&mut MixState, &mut Transient)) -> Mirror {
    let mut state = MixState::default();
    let mut transient = Transient::default();
    f(&mut state, &mut transient);
    let mut m = Mirror::default();
    m.apply(&EngineMsg::State {
        rev: 1,
        state,
        transient,
    });
    m
}

fn input(id: &str) -> Source {
    Source::Input(InputId::new(id))
}

fn mix(id: &str) -> MixId {
    MixId::new(id)
}

#[test]
fn db_and_pan_convert_at_the_edges() {
    assert_eq!(ui_db(DB_OFF), -60.0);
    assert_eq!(ui_db(-60.0), -60.0);
    assert_eq!(ui_db(-59.9), -59.9);
    assert_eq!(ui_db(0.0), 0.0);
    assert_eq!(ui_db(12.04), 12.0);
    assert_eq!(ui_db(f64::NAN), -60.0);
    assert_eq!(engine_db(-60.0), Some(DB_OFF));
    assert_eq!(engine_db(-75.0), Some(DB_OFF));
    assert_eq!(engine_db(-59.8), Some(f64::from(-59.8f32)));
    assert_eq!(engine_db(6.0), Some(6.0));
    assert_eq!(engine_db(20.0), Some(12.0));
    assert_eq!(engine_db(f32::NAN), None);
    assert_eq!(engine_db(f32::INFINITY), None);
    assert_eq!(ui_pan(-1.0), 0.0);
    assert_eq!(ui_pan(0.0), 0.5);
    assert_eq!(ui_pan(1.0), 1.0);
    assert_eq!(ui_pan(3.0), 1.0);
    assert_eq!(engine_pan(0.0), Some(-1.0));
    assert_eq!(engine_pan(0.5), Some(0.0));
    assert_eq!(engine_pan(1.0), Some(1.0));
    assert_eq!(engine_pan(0.75), Some(0.5));
    assert_eq!(engine_pan(1.5), None);
    assert_eq!(engine_pan(f32::NAN), None);
    assert_eq!(limit_db(0.0), -6.0);
    assert_eq!(limit_db(0.5), -3.0);
    assert_eq!(limit_db(1.0), 0.0);
    assert_eq!(limit_norm(-6.0), 0.0);
    assert_eq!(limit_norm(-3.0), 0.5);
    assert_eq!(limit_norm(0.0), 1.0);
    assert_eq!(limit_norm(3.0), 1.0);
}

#[test]
fn channels_follow_the_topology_then_the_heard_mixes() {
    let v = test_view();
    let m = Mirror::default();
    let chs = channels(&v, &m, &page(&v, "member1"), &member("member1"));
    assert_eq!(chs.len(), 24 + 8);
    assert_eq!(chs[0].id, "mic1");
    assert_eq!(chs[0].name, "MEMBER1 mic");
    assert!(chs[0].own && chs[0].eq);
    assert!(!chs[1].own && !chs[1].eq);
    assert_eq!(chs[23].id, "bgvs");
    assert_eq!(chs[23].category, "stems");
    assert_eq!(chs[24].id, "member2");
    assert_eq!(chs[24].name, "Member2");
    assert_eq!(chs[24].category, "mixes");
    assert!(!chs[24].eq);
    // Engine defaults: levels off (−60 on the fader), centred.
    assert!(
        chs.iter()
            .all(|c| c.level_db == -60.0 && c.pan == 0.5 && !c.muted)
    );
    assert_eq!(
        channels(&v, &m, &page(&v, "member2"), &member("member2")).len(),
        24
    );
    let eng = channels(&v, &m, &page(&v, "engineer"), &engineer());
    assert_eq!(eng.len(), 24 + 9);
    assert!(eng.iter().all(|c| c.eq));
    assert!(eng.iter().any(|c| c.own && c.id == "eng_mic"));
    // The engineer on a member's page: that member's own channel, every EQ.
    let on4 = channels(&v, &m, &page(&v, "member4"), &engineer());
    let own: Vec<&str> = on4
        .iter()
        .filter(|c| c.own)
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(own, ["mic4"]);
    assert!(on4.iter().all(|c| c.eq));
    // Member4 owns two inputs: both EQs, one own channel.
    let m4 = channels(&v, &m, &page(&v, "member4"), &member("member4"));
    let eqs: Vec<&str> = m4.iter().filter(|c| c.eq).map(|c| c.id.as_str()).collect();
    assert_eq!(eqs, ["mic4", "mic5"]);
    // The translator page has no own channel.
    let t = channels(&v, &m, &page(&v, "translator"), &engineer());
    assert_eq!(t.len(), 24);
    assert!(t.iter().all(|c| !c.own));
}

#[test]
fn levels_pans_and_the_solo_mask_show_on_the_channels() {
    let v = test_view();
    let m = mirror_with(|s, t| {
        let mx = s.mixes.entry(mix("member1")).or_default();
        mx.inputs.insert(
            InputId::new("mic2"),
            Level {
                gain_db: -6.0,
                pan: -0.5,
                muted: false,
            },
        );
        mx.inputs.insert(
            InputId::new("keys"),
            Level {
                gain_db: 3.0,
                pan: 0.0,
                muted: true,
            },
        );
        mx.mixes.insert(
            mix("member2"),
            Level {
                gain_db: -1.0,
                pan: 0.0,
                muted: false,
            },
        );
        t.solo.push(iem_engine_proto::Solo {
            mix: mix("member1"),
            sources: vec![input("keys"), Source::Mix(mix("member2"))],
        });
    });
    let p = page(&v, "member1");
    let chs = channels(&v, &m, &p, &member("member1"));
    let get = |id: &str| chs.iter().find(|c| c.id == id).unwrap();
    assert_eq!((get("mic2").level_db, get("mic2").pan), (-6.0, 0.25));
    assert!(get("mic2").muted, "masked by the solo");
    assert!(get("keys").muted, "soloed but muted itself");
    assert!(!get("member2").muted, "soloed");
    assert!(get("mic1").muted, "masked");
    // Another page is not masked.
    let other = channels(&v, &m, &page(&v, "member2"), &member("member2"));
    assert!(other.iter().all(|c| !c.muted));
}

#[test]
fn the_state_message_names_the_mix_and_the_stems_strip() {
    let v = test_view();
    let m = mirror_with(|s, _| {
        let mx = s.mixes.entry(mix("member3")).or_default();
        mx.out = MixOut {
            volume_db: -4.0,
            muted: true,
            ..MixOut::default()
        };
        mx.groups.insert(
            GroupId::new("stems"),
            MixGroup {
                gain_db: 2.0,
                muted: true,
                ..MixGroup::default()
            },
        );
    });
    let ServerMsg::State {
        channels,
        connected,
        global_level_db,
        global_muted,
        mix: mx,
        stems_level_db,
        stems_muted,
        group,
    } = state_msg(&v, &m, &page(&v, "member3"), &member("member3"), true)
    else {
        panic!("a State");
    };
    assert_eq!(channels.len(), 24);
    assert!(connected);
    assert_eq!((global_level_db, global_muted), (Some(-4.0), Some(true)));
    assert_eq!(mx.as_deref(), Some("member3"));
    assert_eq!((stems_level_db, stems_muted), (Some(2.0), Some(true)));
    assert_eq!(group.as_deref(), Some("stems"));
    // Without groups the stems fields are absent.
    let mut nogroup = v.clone();
    nogroup.group = None;
    let ServerMsg::State {
        stems_level_db,
        group,
        connected,
        ..
    } = state_msg(
        &nogroup,
        &m,
        &page(&v, "member3"),
        &member("member3"),
        false,
    )
    else {
        panic!("a State");
    };
    assert_eq!((stems_level_db, group, connected), (None, None, false));
}

#[test]
fn changes_become_updates_for_their_page_only() {
    let v = test_view();
    let p1 = page(&v, "member1");
    let level = Level {
        gain_db: -3.0,
        pan: 1.0,
        muted: true,
    };
    let m = mirror_with(|s, _| {
        s.mixes
            .entry(mix("member1"))
            .or_default()
            .inputs
            .insert(InputId::new("mic3"), level);
    });
    let changes = vec![
        Change::Level {
            mix: mix("member1"),
            source: input("mic3"),
            level,
        },
        Change::Level {
            mix: mix("member2"),
            source: input("mic3"),
            level,
        },
        Change::MixOut {
            mix: mix("member1"),
            out: MixOut {
                volume_db: DB_OFF,
                muted: false,
                ..MixOut::default()
            },
        },
        Change::Group {
            mix: mix("member1"),
            group: GroupId::new("stems"),
            state: MixGroup {
                gain_db: -9.0,
                muted: true,
                ..MixGroup::default()
            },
        },
        Change::Group {
            mix: mix("member1"),
            group: GroupId::new("other"),
            state: MixGroup::default(),
        },
        Change::Input {
            id: InputId::new("mic3"),
            state: iem_engine_proto::InputState::default(),
        },
        Change::LimiterStatsReset {
            mix: mix("member1"),
        },
    ];
    assert_eq!(
        updates_for(&v, &m, &p1, &changes),
        vec![
            ServerMsg::ChannelUpdate {
                id: "mic3".into(),
                level_db: -3.0,
                muted: true,
                pan: 1.0
            },
            ServerMsg::GlobalVolumeUpdate {
                level_db: -60.0,
                muted: false
            },
            ServerMsg::StemsVolumeUpdate {
                level_db: -9.0,
                muted: true
            },
        ]
    );
    assert!(updates_for(&v, &m, &page(&v, "member3"), &changes).is_empty());
    // A heard mix the page does not hear is not shown.
    let heard = [Change::Level {
        mix: mix("member2"),
        source: Source::Mix(mix("member3")),
        level,
    }];
    assert!(updates_for(&v, &m, &page(&v, "member2"), &heard).is_empty());
}

#[test]
fn a_solo_change_sends_the_solo_and_every_channel_again() {
    let v = test_view();
    let m = mirror_with(|_, t| {
        t.solo.push(iem_engine_proto::Solo {
            mix: mix("member2"),
            sources: vec![input("mic2")],
        });
    });
    let p = page(&v, "member2");
    let ups = updates_for(
        &v,
        &m,
        &p,
        &[Change::Solo {
            mix: mix("member2"),
            sources: vec![input("mic2")],
        }],
    );
    assert_eq!(
        ups[0],
        ServerMsg::SoloUpdate {
            soloed: vec!["mic2".into()]
        }
    );
    assert_eq!(ups.len(), 1 + 24);
    let muted: Vec<bool> = ups[1..]
        .iter()
        .map(|u| match u {
            ServerMsg::ChannelUpdate { muted, .. } => *muted,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(muted.iter().filter(|m| !**m).count(), 1);
}

#[test]
fn another_mixs_solo_changes_nothing_on_the_page() {
    let v = test_view();
    let m = mirror_with(|_, t| {
        t.solo.push(iem_engine_proto::Solo {
            mix: mix("member2"),
            sources: vec![input("mic2")],
        });
    });
    let solo = [Change::Solo {
        mix: mix("member2"),
        sources: vec![input("mic2")],
    }];
    assert!(updates_for(&v, &m, &page(&v, "member3"), &solo).is_empty());
}

#[test]
fn ui_commands_become_engine_commands_on_the_page_mix() {
    let v = test_view();
    let m = Mirror::default();
    let p = page(&v, "member1");
    let me = member("member1");
    let one = |msg: ClientMsg| {
        let mut c = command(&v, &m, &p, &me, &msg).unwrap();
        assert_eq!(c.len(), 1);
        c.remove(0)
    };
    assert_eq!(
        one(ClientMsg::SetLevel {
            id: "mic2".into(),
            level_db: -60.0
        }),
        Cmd::SetLevel {
            mix: mix("member1"),
            source: input("mic2"),
            gain_db: Some(DB_OFF),
            pan: None,
            muted: None
        }
    );
    assert_eq!(
        one(ClientMsg::SetMute {
            id: "member2".into(),
            muted: true
        }),
        Cmd::SetLevel {
            mix: mix("member1"),
            source: Source::Mix(mix("member2")),
            gain_db: None,
            pan: None,
            muted: Some(true)
        }
    );
    assert_eq!(
        one(ClientMsg::SetPan {
            id: "keys".into(),
            pan: 0.25
        }),
        Cmd::SetLevel {
            mix: mix("member1"),
            source: input("keys"),
            gain_db: None,
            pan: Some(-0.5),
            muted: None
        }
    );
    assert_eq!(
        one(ClientMsg::SetGlobalLevel { level_db: -3.0 }),
        Cmd::SetMix {
            mix: mix("member1"),
            volume_db: Some(-3.0),
            muted: None
        }
    );
    assert_eq!(
        one(ClientMsg::SetGlobalMute { muted: true }),
        Cmd::SetMix {
            mix: mix("member1"),
            volume_db: None,
            muted: Some(true)
        }
    );
    assert_eq!(
        one(ClientMsg::SetStemsLevel { level_db: 1.0 }),
        Cmd::SetGroup {
            mix: mix("member1"),
            group: GroupId::new("stems"),
            gain_db: Some(1.0),
            muted: None
        }
    );
    assert_eq!(
        one(ClientMsg::SetStemsMute { muted: false }),
        Cmd::SetGroup {
            mix: mix("member1"),
            group: GroupId::new("stems"),
            gain_db: None,
            muted: Some(false)
        }
    );
    assert_eq!(
        one(ClientMsg::SetSolo {
            soloed: vec!["mic1".into(), "member3".into()]
        }),
        Cmd::SetSolo {
            mix: mix("member1"),
            sources: vec![input("mic1"), Source::Mix(mix("member3"))]
        }
    );
    assert_eq!(
        one(ClientMsg::SetLimiterParam {
            param: "limit".into(),
            value: 0.5
        }),
        Cmd::SetLimiter {
            mix: mix("member1"),
            enabled: None,
            limit_db: Some(-3.0)
        }
    );
    assert_eq!(
        one(ClientMsg::SetLimiterEnabled { enabled: false }),
        Cmd::SetLimiter {
            mix: mix("member1"),
            enabled: Some(false),
            limit_db: None
        }
    );
    assert_eq!(
        one(ClientMsg::ResetLimiterActivity),
        Cmd::ResetLimiterStats {
            mix: mix("member1")
        }
    );
    let Cmd::SetEq { target, eq } = one(ClientMsg::SetEqBand {
        target: "mic1".into(),
        band: 2,
        param: "gain_db".into(),
        value: 4.5,
    }) else {
        panic!("SetEq");
    };
    assert_eq!(target, EqTarget::Input(InputId::new("mic1")));
    assert_eq!(eq.bands[2].gain_db, 4.5);
}

#[test]
fn bad_values_unknown_ids_and_foreign_targets_are_refused() {
    let v = test_view();
    let m = Mirror::default();
    let p2 = page(&v, "member2");
    let me = member("member2");
    let err = |msg: ClientMsg| command(&v, &m, &p2, &me, &msg).unwrap_err();
    assert_eq!(
        err(ClientMsg::SetLevel {
            id: "member3".into(),
            level_db: 0.0
        }),
        ViewError::UnknownId("member3".into()),
        "member2 hears no other mix"
    );
    assert_eq!(
        err(ClientMsg::SetMute {
            id: "stems".into(),
            muted: true
        }),
        ViewError::UnknownId("stems".into())
    );
    assert!(matches!(
        err(ClientMsg::SetLevel {
            id: "mic1".into(),
            level_db: f32::NAN
        }),
        ViewError::BadValue(_)
    ));
    assert!(matches!(
        err(ClientMsg::SetPan {
            id: "mic1".into(),
            pan: -0.5
        }),
        ViewError::BadValue(_)
    ));
    assert!(matches!(
        err(ClientMsg::SetGlobalLevel {
            level_db: f32::INFINITY
        }),
        ViewError::BadValue(_)
    ));
    assert!(matches!(
        err(ClientMsg::SetSolo {
            soloed: vec!["mic1".into(), "nope".into()]
        }),
        ViewError::UnknownId(_)
    ));
    assert!(matches!(
        err(ClientMsg::SetLimiterParam {
            param: "release".into(),
            value: 0.5
        }),
        ViewError::BadValue(_)
    ));
    assert!(matches!(
        err(ClientMsg::SetLimiterParam {
            param: "limit".into(),
            value: 1.5
        }),
        ViewError::BadValue(_)
    ));
    assert_eq!(
        err(ClientMsg::SetEqBand {
            target: "mic1".into(),
            band: 0,
            param: "gain_db".into(),
            value: 1.0
        }),
        ViewError::Forbidden("an input's EQ is its owner's or the engineer's")
    );
    assert_eq!(
        err(ClientMsg::SetInput {
            input: "mic2".into(),
            trim_db: Some(1.0),
            muted: None,
            processing: None
        }),
        ViewError::Forbidden("input controls are the engineer's")
    );
    assert_eq!(
        err(ClientMsg::ResetLimiterStats {
            mix: "member2".into()
        }),
        ViewError::Forbidden("another mix's limiter is the engineer's")
    );
    assert_eq!(err(ClientMsg::GetConsole), ViewError::NotACommand);
    assert_eq!(err(ClientMsg::TalkStart), ViewError::NotACommand);
    let mut nogroup = v.clone();
    nogroup.group = None;
    assert_eq!(
        command(
            &nogroup,
            &m,
            &p2,
            &me,
            &ClientMsg::SetStemsMute { muted: true }
        ),
        Err(ViewError::NoGroup)
    );
}

#[test]
fn a_refused_pan_names_its_value() {
    let v = test_view();
    let m = Mirror::default();
    let p2 = page(&v, "member2");
    let refused = |pan: f32| {
        command(
            &v,
            &m,
            &p2,
            &member("member2"),
            &ClientMsg::SetPan {
                id: "mic1".into(),
                pan,
            },
        )
        .unwrap_err()
        .to_string()
    };
    // The UI pan is 0…1; the refusal (logged) names what was sent.
    assert_eq!(refused(1.5), "bad value: pan 1.5");
    assert_eq!(refused(-0.25), "bad value: pan -0.25");
    assert_eq!(refused(f32::NAN), "bad value: pan NaN");
    assert_eq!(refused(1.0001), "bad value: pan 1.0001");
}

#[test]
fn engineer_only_commands_work_for_the_engineer() {
    let v = test_view();
    let m = Mirror::default();
    let p = page(&v, "engineer");
    let e = engineer();
    assert_eq!(
        command(
            &v,
            &m,
            &p,
            &e,
            &ClientMsg::SetInput {
                input: "keys".into(),
                trim_db: Some(-3.0),
                muted: Some(true),
                processing: Some(false)
            }
        ),
        Ok(vec![Cmd::SetInput {
            input: InputId::new("keys"),
            trim_db: Some(-3.0),
            muted: Some(true),
            processing: Some(false)
        }])
    );
    assert!(matches!(
        command(
            &v,
            &m,
            &p,
            &e,
            &ClientMsg::SetInput {
                input: "keys".into(),
                trim_db: Some(f32::NAN),
                muted: None,
                processing: None
            }
        ),
        Err(ViewError::BadValue(_))
    ));
    assert_eq!(
        command(
            &v,
            &m,
            &p,
            &e,
            &ClientMsg::SetInput {
                input: "nope".into(),
                trim_db: None,
                muted: None,
                processing: None
            }
        ),
        Err(ViewError::UnknownId("nope".into()))
    );
    assert_eq!(
        command(
            &v,
            &m,
            &p,
            &e,
            &ClientMsg::ResetLimiterStats {
                mix: "translator".into()
            }
        ),
        Ok(vec![Cmd::ResetLimiterStats {
            mix: mix("translator")
        }])
    );
    assert_eq!(
        command(
            &v,
            &m,
            &p,
            &e,
            &ClientMsg::ResetLimiterStats { mix: "x".into() }
        ),
        Err(ViewError::UnknownId("x".into()))
    );
}

#[test]
fn eq_targets_follow_x7() {
    let v = test_view();
    let p1 = page(&v, "member1");
    let me = member("member1");
    assert_eq!(
        eq_target(&v, &p1, &me, "member1"),
        Ok((EqTarget::Mix(mix("member1")), "IEM VOL".into()))
    );
    assert_eq!(
        eq_target(&v, &p1, &me, "stems"),
        Ok((
            EqTarget::Group {
                mix: mix("member1"),
                group: GroupId::new("stems")
            },
            "STEMS".into()
        ))
    );
    assert_eq!(
        eq_target(&v, &p1, &me, "mic1"),
        Ok((EqTarget::Input(InputId::new("mic1")), "MEMBER1 mic".into()))
    );
    assert!(matches!(
        eq_target(&v, &p1, &me, "mic2"),
        Err(ViewError::Forbidden(_))
    ));
    assert!(matches!(
        eq_target(&v, &p1, &me, "member2"),
        Err(ViewError::Forbidden(_))
    ));
    assert_eq!(
        eq_target(&v, &p1, &engineer(), "member2"),
        Ok((EqTarget::Mix(mix("member2")), "Member2".into()))
    );
    assert_eq!(
        eq_target(&v, &p1, &engineer(), "mic2").map(|t| t.0),
        Ok(EqTarget::Input(InputId::new("mic2")))
    );
    assert_eq!(
        eq_target(&v, &p1, &me, "engineer"),
        Err(ViewError::UnknownId("engineer".into()))
    );
}

#[test]
fn eq_edits_compose_and_map_to_the_ui_bands() {
    let eq = Eq::default();
    let a = apply_band(&eq, 3, "freq_hz", 2500.0).unwrap();
    let b = apply_band(&a, 3, "gain_db", -4.0).unwrap();
    let c = apply_band(&b, 3, "bw_oct", 0.5).unwrap();
    let d = apply_band(&c, 3, "enabled", 1.0).unwrap();
    assert_eq!(
        (
            d.bands[3].freq_hz,
            d.bands[3].gain_db,
            d.bands[3].bw_oct,
            d.bands[3].enabled
        ),
        (2500.0, -4.0, 0.5, true)
    );
    assert!(!apply_band(&d, 3, "enabled", 0.49).unwrap().bands[3].enabled);
    assert_eq!(d.bands[2], eq.bands[2]);
    assert!(apply_band(&eq, 5, "gain_db", 0.0).is_err());
    assert!(apply_band(&eq, 0, "gain", 0.25).is_err());
    assert!(apply_band(&eq, 0, "gain_db", f32::NAN).is_err());
    let ui = eq_bands(&d);
    let kinds: Vec<&str> = ui.iter().map(|b| b.band_type.as_str()).collect();
    assert_eq!(kinds, ["highpass", "lowshelf", "band", "band", "highshelf"]);
    assert_eq!(
        (ui[3].freq_hz, ui[3].gain_db, ui[3].bw, ui[3].enabled),
        (2500.0, -4.0, 0.5, true)
    );
    let mut off = eq;
    off.bands[1].gain_db = -1000.0;
    assert_eq!(eq_bands(&off)[1].gain_db, -150.0);
}

#[test]
fn a_gain_change_leaves_a_disabled_high_pass_off() {
    // The high-pass has no gain (the engine ignores it): neither a gain
    // drag nor Reset (gain 0) may switch on an audible low cut.
    let eq = Eq::default();
    assert_eq!(
        (eq.bands[0].kind, eq.bands[0].enabled),
        (BandKind::HighPass, false)
    );
    for gain in [0.0_f32, 3.0] {
        let moved = apply_band(&eq, 0, "gain_db", gain).unwrap();
        assert_eq!(
            (moved.bands[0].gain_db, moved.bands[0].enabled),
            (f64::from(gain), false),
            "{gain}"
        );
    }
    // Its own switch still works, and a gain change leaves it on.
    let on = apply_band(&eq, 0, "enabled", 1.0).unwrap();
    assert!(apply_band(&on, 0, "gain_db", 3.0).unwrap().bands[0].enabled);
    // Every band with a gain switches on, Reset's gain 0 included.
    for band in 1..5_u8 {
        let reset = apply_band(&eq, band, "gain_db", 0.0).unwrap();
        assert!(reset.bands[usize::from(band)].enabled, "band {band}");
    }
}

#[test]
fn a_gain_change_enables_a_disabled_band() {
    // ReaEQ's behaviour, kept from gen1 (P9: the band notices nothing):
    // moving the gain of a disabled band switches the band on.
    let eq = Eq::default();
    assert!(!eq.bands[1].enabled);
    let moved = apply_band(&eq, 1, "gain_db", 3.0).unwrap();
    assert_eq!(
        (moved.bands[1].gain_db, moved.bands[1].enabled),
        (3.0, true)
    );
    // Only the gain does: frequency and width leave the switch alone.
    assert!(!apply_band(&eq, 1, "freq_hz", 250.0).unwrap().bands[1].enabled);
    assert!(!apply_band(&eq, 1, "bw_oct", 0.5).unwrap().bands[1].enabled);
    // An enabled band stays on; the other bands are untouched.
    let again = apply_band(&moved, 1, "gain_db", -2.0).unwrap();
    assert_eq!(
        (again.bands[1].gain_db, again.bands[1].enabled),
        (-2.0, true)
    );
    assert_eq!(again.bands[..1], eq.bands[..1]);
    assert_eq!(again.bands[2..], eq.bands[2..]);
    // The page command sends the band switched on to the engine.
    let v = test_view();
    let m = Mirror::default();
    let p1 = page(&v, "member1");
    let cmds = command(
        &v,
        &m,
        &p1,
        &member("member1"),
        &ClientMsg::SetEqBand {
            target: "member1".into(),
            band: 1,
            param: "gain_db".into(),
            value: 3.0,
        },
    )
    .unwrap();
    match cmds.as_slice() {
        [Cmd::SetEq { target, eq }] => {
            assert_eq!(target, &EqTarget::Mix(mix("member1")));
            assert_eq!((eq.bands[1].gain_db, eq.bands[1].enabled), (3.0, true));
        }
        other => panic!("one SetEq, got {other:?}"),
    }
}

#[test]
fn eq_and_limiter_messages_read_the_mirror() {
    let v = test_view();
    let m = mirror_with(|s, _| {
        let mx = s.mixes.entry(mix("member1")).or_default();
        mx.out.limiter = Limiter {
            enabled: false,
            limit_db: -1.5,
        };
        mx.out.eq.bands[0].enabled = true;
        s.inputs.entry(InputId::new("mic1")).or_default().eq.bands[4].gain_db = 2.0;
        mx.groups.entry(GroupId::new("stems")).or_default().eq.bands[1].freq_hz = 150.0;
    });
    let p = page(&v, "member1");
    let me = member("member1");
    let ServerMsg::EqParams {
        target,
        track_name,
        bands,
    } = eq_params_msg(&v, &m, &p, &me, "mic1").unwrap()
    else {
        panic!("EqParams");
    };
    assert_eq!(
        (target.as_str(), track_name.as_str()),
        ("mic1", "MEMBER1 mic")
    );
    assert_eq!(bands[4].gain_db, 2.0);
    let ServerMsg::EqParams { bands, .. } = eq_params_msg(&v, &m, &p, &me, "member1").unwrap()
    else {
        panic!("EqParams");
    };
    assert!(bands[0].enabled);
    let ServerMsg::EqParams { bands, .. } = eq_params_msg(&v, &m, &p, &me, "stems").unwrap() else {
        panic!("EqParams");
    };
    assert_eq!(bands[1].freq_hz, 150.0);
    assert!(eq_params_msg(&v, &m, &p, &me, "mic9").is_err());
    assert_eq!(
        limiter_msg(&m, &p, 12.5),
        ServerMsg::LimiterParams {
            mix: "member1".into(),
            track_name: "IEM VOL".into(),
            limit_db: -1.5,
            limit_norm: 0.75,
            enabled: false,
            active_seconds: 12.5
        }
    );
    // The current EQ of a group and a heard mix come from their entities.
    assert_eq!(
        current_eq(
            &m,
            &EqTarget::Group {
                mix: mix("member1"),
                group: GroupId::new("stems")
            }
        )
        .bands[1]
            .freq_hz,
        150.0
    );
    assert_eq!(
        current_eq(&m, &EqTarget::Mix(mix("member2"))),
        Eq::default()
    );
}

#[test]
fn mute_all_mutes_every_level_of_the_page_mix() {
    let v = test_view();
    let Cmd::Batch { ops } = mute_all(&v, &page(&v, "engineer")) else {
        panic!("a batch");
    };
    assert_eq!(ops.len(), 24 + 9);
    assert!(ops.iter().all(|c| matches!(
        c,
        Cmd::SetLevel { mix, muted: Some(true), gain_db: None, pan: None, .. } if mix.0 == "engineer"
    )));
    assert!(ops.contains(&Cmd::SetLevel {
        mix: mix("engineer"),
        source: Source::Mix(mix("member1")),
        gain_db: None,
        pan: None,
        muted: Some(true)
    }));
    let Cmd::Batch { ops } = mute_all(&v, &page(&v, "member5")) else {
        panic!("a batch");
    };
    assert_eq!(ops.len(), 24);
}
