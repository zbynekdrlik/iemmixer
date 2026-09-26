use super::*;
use crate::test_support::test_site;
use iem_engine_proto::{DB_OFF, Eq, Limiter};

fn core(flags: Flags) -> Core {
    Core::new(Arc::new(test_site()), &MixState::default(), 0, flags)
}

fn mix(s: &str) -> MixId {
    MixId::new(s)
}

fn input(s: &str) -> InputId {
    InputId::new(s)
}

fn stems() -> GroupId {
    GroupId::new("stems")
}

fn set_mix(name: &str, volume_db: Option<f64>, muted: Option<bool>) -> Cmd {
    Cmd::SetMix {
        mix: mix(name),
        volume_db,
        muted,
    }
}

fn set_level(name: &str, source: Source, gain_db: f64) -> Cmd {
    Cmd::SetLevel {
        mix: mix(name),
        source,
        gain_db: Some(gain_db),
        pan: None,
        muted: None,
    }
}

fn solo(name: &str, sources: Vec<Source>) -> Cmd {
    Cmd::SetSolo {
        mix: mix(name),
        sources,
    }
}

fn src(s: &str) -> Source {
    Source::Input(input(s))
}

fn code(r: Result<Outcome, CmdError>) -> ErrCode {
    r.unwrap_err().code
}

#[test]
fn a_set_changes_state_bumps_rev_and_emits_one_rt_op() {
    let mut c = core(Flags::default());
    let out = c.apply(&set_mix("member1", Some(-3.0), None)).unwrap();
    assert_eq!(out.rev, 1);
    assert_eq!(out.effect, Effect::None);
    let m = c.topology().mix_index(&mix("member1")).unwrap();
    let state = MixOut {
        volume_db: -3.0,
        ..MixOut::default()
    };
    assert_eq!(
        out.changes,
        vec![Change::MixOut {
            mix: mix("member1"),
            out: state
        }]
    );
    assert_eq!(
        out.rt,
        vec![RtOp::MixOut {
            m: m as u16,
            volume: 10f64.powf(-3.0 / 20.0),
            muted: false
        }]
    );
    assert_eq!(c.state().mixes[&mix("member1")].out, state);
    assert_eq!(c.rev(), 1);
    // A level: one change carrying the source, one RT op on its slot.
    let out = c.apply(&set_level("member1", src("keys"), -6.0)).unwrap();
    let level = Level {
        gain_db: -6.0,
        ..Level::default()
    };
    assert_eq!(
        out.changes,
        vec![Change::Level {
            mix: mix("member1"),
            source: src("keys"),
            level
        }]
    );
    assert_eq!(
        out.rt,
        vec![RtOp::Level {
            m: m as u16,
            k: 14,
            gain: 10f64.powf(-6.0 / 20.0),
            pan: 0.0,
            muted: false
        }]
    );
    assert_eq!(
        c.state().mixes[&mix("member1")].inputs[&input("keys")],
        level
    );
    // A heard mix's level lands after the inputs.
    let out = c
        .apply(&set_level("member1", Source::Mix(mix("member2")), 0.0))
        .unwrap();
    assert!(matches!(out.rt.as_slice(), [RtOp::Level { k: 24, gain, .. }] if *gain == 1.0));
    assert_eq!(
        c.state().mixes[&mix("member1")].mixes[&mix("member2")].gain_db,
        0.0
    );
    // A group strip.
    let out = c
        .apply(&Cmd::SetGroup {
            mix: mix("member1"),
            group: stems(),
            gain_db: Some(-10.0),
            muted: Some(true),
        })
        .unwrap();
    assert_eq!(
        out.rt,
        vec![RtOp::Group {
            m: m as u16,
            g: 0,
            gain: 10f64.powf(-0.5),
            muted: true
        }]
    );
    assert_eq!(
        out.changes,
        vec![Change::Group {
            mix: mix("member1"),
            group: stems(),
            state: MixGroup {
                gain_db: -10.0,
                muted: true,
                eq: Eq::default()
            }
        }]
    );
    assert_eq!(c.rev(), 4);
}

#[test]
fn an_unchanged_set_keeps_rev() {
    let mut c = core(Flags::default());
    c.apply(&set_mix("member1", Some(-3.0), None)).unwrap();
    let out = c.apply(&set_mix("member1", Some(-3.0), None)).unwrap();
    assert_eq!((out.rev, out.changes.len(), out.rt.len()), (1, 0, 0));
    let none = c
        .apply(&Cmd::SetInput {
            input: input("mic1"),
            trim_db: None,
            muted: None,
            processing: None,
        })
        .unwrap();
    assert_eq!((none.rev, none.changes.len()), (1, 0));
    let off = c.apply(&set_level("member2", src("mic1"), DB_OFF)).unwrap();
    assert_eq!((off.rev, off.changes.len(), off.rt.len()), (1, 0, 0));
    let same = c
        .apply(&Cmd::SetGroup {
            mix: mix("member2"),
            group: stems(),
            gain_db: Some(0.0),
            muted: Some(false),
        })
        .unwrap();
    assert_eq!((same.rev, same.changes.len()), (1, 0));
}

#[test]
fn values_are_capped_and_non_finite_rejected() {
    let mut c = core(Flags::default());
    c.apply(&set_mix("member2", Some(40.0), None)).unwrap();
    assert_eq!(c.state().mixes[&mix("member2")].out.volume_db, 12.0);
    let before = c.state();
    assert_eq!(
        code(c.apply(&set_mix("member2", Some(f64::NAN), None))),
        ErrCode::BadValue
    );
    assert_eq!(c.state(), before);
    assert_eq!(c.rev(), 1);
    c.apply(&Cmd::SetInput {
        input: input("mic1"),
        trim_db: Some(1e308),
        muted: Some(true),
        processing: Some(false),
    })
    .unwrap();
    let i = c.state().inputs[&input("mic1")];
    assert_eq!((i.trim_db, i.muted, i.processing), (24.0, true, false));
    assert_eq!(
        code(c.apply(&Cmd::SetInput {
            input: input("mic1"),
            trim_db: Some(f64::NAN),
            muted: None,
            processing: None,
        })),
        ErrCode::BadValue
    );
    c.apply(&Cmd::SetLevel {
        mix: mix("member1"),
        source: src("mic1"),
        gain_db: Some(99.0),
        pan: Some(-3.0),
        muted: None,
    })
    .unwrap();
    let l = c.state().mixes[&mix("member1")].inputs[&input("mic1")];
    assert_eq!((l.gain_db, l.pan), (12.0, -1.0));
    for bad in [
        Cmd::SetLevel {
            mix: mix("member1"),
            source: src("mic1"),
            gain_db: None,
            pan: Some(f64::NAN),
            muted: None,
        },
        Cmd::SetLevel {
            mix: mix("member1"),
            source: src("mic1"),
            gain_db: Some(f64::INFINITY),
            pan: None,
            muted: None,
        },
        Cmd::SetGroup {
            mix: mix("member1"),
            group: stems(),
            gain_db: Some(f64::NAN),
            muted: None,
        },
        Cmd::SetLimiter {
            mix: mix("member1"),
            enabled: None,
            limit_db: Some(f64::NAN),
        },
    ] {
        assert_eq!(code(c.apply(&bad)), ErrCode::BadValue, "{bad:?}");
    }
    c.apply(&Cmd::SetGroup {
        mix: mix("member1"),
        group: stems(),
        gain_db: Some(50.0),
        muted: None,
    })
    .unwrap();
    assert_eq!(
        c.state().mixes[&mix("member1")].groups[&stems()].gain_db,
        12.0
    );
    let mut eq = Eq::default();
    eq.bands[2].freq_hz = f64::NAN;
    for target in [
        EqTarget::Input(input("mic1")),
        EqTarget::Mix(mix("member1")),
        EqTarget::Group {
            mix: mix("member1"),
            group: stems(),
        },
    ] {
        assert_eq!(code(c.apply(&Cmd::SetEq { target, eq })), ErrCode::BadValue);
    }
    eq.bands[2].freq_hz = 1e7;
    eq.bands[2].enabled = true;
    let out = c
        .apply(&Cmd::SetEq {
            target: EqTarget::Input(input("mic1")),
            eq,
        })
        .unwrap();
    assert_eq!(
        c.state().inputs[&input("mic1")].eq.bands[2].freq_hz,
        24_000.0
    );
    assert!(matches!(out.rt.as_slice(), [RtOp::InputEq { i: 0, .. }]));
}

#[test]
fn unknown_ids_are_unknown_id_with_short_messages() {
    let mut c = core(Flags::default());
    let long = "x".repeat(10_000);
    let cases = vec![
        set_mix("nope", Some(0.0), None),
        set_mix(&long, Some(0.0), None),
        Cmd::SetInput {
            input: input(&long),
            trim_db: None,
            muted: None,
            processing: None,
        },
        // member3 hears no other mix; nobody hears the translator.
        set_level("member3", Source::Mix(mix("member2")), 0.0),
        set_level("engineer", Source::Mix(mix("translator")), 0.0),
        set_level("member1", Source::Mix(mix(&long)), 0.0),
        set_level("member1", src(&long), 0.0),
        set_level(&long, src("mic1"), 0.0),
        Cmd::SetGroup {
            mix: mix("member1"),
            group: GroupId::new("nope"),
            gain_db: None,
            muted: None,
        },
        Cmd::SetEq {
            target: EqTarget::Mix(mix("nope")),
            eq: Eq::default(),
        },
        Cmd::SetEq {
            target: EqTarget::Group {
                mix: mix("member1"),
                group: GroupId::new(long.clone()),
            },
            eq: Eq::default(),
        },
        Cmd::SetLimiter {
            mix: mix("nope"),
            enabled: None,
            limit_db: None,
        },
        Cmd::ResetLimiterStats { mix: mix("nope") },
        Cmd::StartListen { mix: mix("nope") },
        Cmd::StopListen { mix: mix("nope") },
        solo("nope", vec![]),
    ];
    for cmd in cases {
        let e = c.apply(&cmd).unwrap_err();
        assert_eq!(e.code, ErrCode::UnknownId, "{cmd:?}");
        assert!(e.msg.len() < 160, "{}", e.msg.len());
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
            set_mix("member1", Some(-1.0), None),
            set_mix("member2", Some(-2.0), None),
        ],
    };
    let out = c.apply(&good).unwrap();
    assert_eq!((out.rev, out.changes.len(), out.rt.len()), (1, 2, 2));
    let failing = Cmd::Batch {
        ops: vec![
            set_mix("member3", Some(-3.0), None),
            set_mix("member4", Some(-4.0), None),
            set_mix("member5", Some(f64::NAN), None),
        ],
    };
    assert_eq!(code(c.apply(&failing)), ErrCode::BadValue);
    assert_eq!(c.state().mixes[&mix("member3")].out.volume_db, 0.0);
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
    // 256 solo toggles on the engineer need more than 512 RT commands.
    let flip = |on: bool| solo("engineer", if on { vec![src("mic1")] } else { vec![] });
    let solos = Cmd::Batch {
        ops: (0..MAX_BATCH).map(|k| flip(k % 2 == 0)).collect(),
    };
    assert_eq!(code(c.apply(&solos)), ErrCode::BadValue);
    assert!(c.transient().solo.is_empty());
    // Exactly 256 commands are one batch.
    let full = Cmd::Batch {
        ops: (1..=MAX_BATCH)
            .map(|k| set_mix("member4", Some(-0.1 * k as f64), None))
            .collect(),
    };
    let out = c.apply(&full).unwrap();
    assert_eq!(
        (out.rev, out.changes.len(), out.rt.len()),
        (2, MAX_BATCH, MAX_BATCH)
    );
    // Exactly 512 RT commands fit one block: 20 solo toggles on member3
    // (24 levels each) and 32 volume moves.
    let member3 = |on: bool| solo("member3", if on { vec![src("mic2")] } else { vec![] });
    let mut ops: Vec<Cmd> = (0..20).map(|k| member3(k % 2 == 0)).collect();
    ops.extend((1..=32).map(|k| set_mix("member6", Some(-f64::from(k)), None)));
    let out = c.apply(&Cmd::Batch { ops }).unwrap();
    assert_eq!((out.rev, out.rt.len()), (3, MAX_CMDS_PER_BLOCK));
    assert!(c.transient().solo.is_empty());
}

#[test]
fn solo_silences_the_other_levels_of_its_mix_and_clears() {
    let mut c = core(Flags::default());
    let t = Arc::clone(c.topology());
    let m3 = t.mix_index(&mix("member3")).unwrap() as u16;
    let out = c.apply(&solo("member3", vec![src("mic2")])).unwrap();
    assert_eq!(out.rev, 1);
    assert_eq!(
        out.changes,
        vec![Change::Solo {
            mix: mix("member3"),
            sources: vec![src("mic2")]
        }]
    );
    // Every input level of member3 (it hears no mix), only mic2 open; the
    // group strip and the output are untouched.
    assert_eq!(out.rt.len(), 24);
    for op in &out.rt {
        let RtOp::Level { m, k, muted, .. } = *op else {
            panic!("{op:?}")
        };
        assert_eq!(m, m3);
        assert_eq!(muted, k != 1, "slot {k}");
    }
    assert_eq!(c.transient().solo.len(), 1);
    // The stored levels are untouched.
    assert!(
        c.state().mixes[&mix("member3")]
            .inputs
            .values()
            .all(|l| !l.muted)
    );
    // A level of another mix is not affected by the solo; one of this mix is.
    let other = c.apply(&set_level("member4", src("mic1"), -6.0)).unwrap();
    assert!(matches!(
        other.rt.as_slice(),
        [RtOp::Level { muted: false, .. }]
    ));
    let inside = c.apply(&set_level("member3", src("mic1"), -6.0)).unwrap();
    assert!(matches!(
        inside.rt.as_slice(),
        [RtOp::Level { muted: true, .. }]
    ));
    let open = c.apply(&set_level("member3", src("mic2"), -6.0)).unwrap();
    assert!(matches!(
        open.rt.as_slice(),
        [RtOp::Level { muted: false, .. }]
    ));
    // A stems input can be soloed like any input.
    c.apply(&solo("member3", vec![src("drums")])).unwrap();
    let same = c.apply(&solo("member3", vec![src("drums")])).unwrap();
    assert_eq!(same.changes.len(), 0);
    let cleared = c.clear_solos();
    assert_eq!(cleared.rev, 6);
    assert_eq!(
        cleared.changes,
        vec![Change::Solo {
            mix: mix("member3"),
            sources: vec![]
        }]
    );
    assert!(
        cleared
            .rt
            .iter()
            .all(|op| matches!(op, RtOp::Level { muted: false, .. }))
    );
    assert_eq!(cleared.rt.len(), 24);
    assert!(c.transient().solo.is_empty());
    assert_eq!(c.clear_solos().rev, 6);
    // A heard mix: soloing member2 on member1 silences the inputs and the
    // other heard mixes.
    let m1 = t.mix_index(&mix("member1")).unwrap();
    let k2 = t.slot(m1, &Source::Mix(mix("member2"))).unwrap();
    let out = c
        .apply(&solo("member1", vec![Source::Mix(mix("member2"))]))
        .unwrap();
    assert_eq!(out.rt.len(), 32);
    for op in &out.rt {
        let RtOp::Level { k, muted, .. } = *op else {
            panic!("{op:?}")
        };
        assert_eq!(muted, usize::from(k) != k2, "slot {k}");
    }
    assert_eq!(
        c.transient().solo,
        vec![Solo {
            mix: mix("member1"),
            sources: vec![Source::Mix(mix("member2"))]
        }]
    );
    let off = c.apply(&solo("member1", vec![])).unwrap();
    assert_eq!(
        off.changes,
        vec![Change::Solo {
            mix: mix("member1"),
            sources: vec![]
        }]
    );
    assert!(c.transient().solo.is_empty());
}

#[test]
fn solo_rejects_sources_the_mix_does_not_hear() {
    let mut c = core(Flags::default());
    for (m, source) in [
        ("member3", src("drums2")),
        ("member3", Source::Mix(mix("member2"))),
        ("member1", Source::Mix(mix("member1"))),
        ("translator", Source::Mix(mix("member1"))),
    ] {
        let e = c.apply(&solo(m, vec![source])).unwrap_err();
        assert_eq!(e.code, ErrCode::BadValue);
    }
    let many = solo("member3", vec![src("mic1"); MAX_SOLO + 1]);
    assert_eq!(code(c.apply(&many)), ErrCode::BadValue);
    let dup = solo("member3", vec![src("mic1"); MAX_SOLO]);
    assert_eq!(c.apply(&dup).unwrap().changes.len(), 1);
    // Every mix can be soloed in, the translator included.
    let tr = c.apply(&solo("translator", vec![src("hand1")])).unwrap();
    assert_eq!(tr.rt.len(), 24);
}

#[test]
fn listen_allows_the_engineer_and_one_other_mix() {
    let mut c = core(Flags::default());
    let t = Arc::clone(c.topology());
    let eng = t.engineer as u16;
    let m4 = t.mix_index(&mix("member4")).unwrap() as u16;
    let out = c
        .apply(&Cmd::StartListen {
            mix: mix("engineer"),
        })
        .unwrap();
    assert_eq!(
        out.rt,
        vec![RtOp::Listen {
            slot: 0,
            mix: Some(eng)
        }]
    );
    assert_eq!(
        out.changes,
        vec![Change::Listen {
            listen: [Some(mix("engineer")), None]
        }]
    );
    let out = c
        .apply(&Cmd::StartListen {
            mix: mix("member4"),
        })
        .unwrap();
    assert_eq!(
        out.rt,
        vec![RtOp::Listen {
            slot: 1,
            mix: Some(m4)
        }]
    );
    assert_eq!(
        c.apply(&Cmd::StartListen {
            mix: mix("member4")
        })
        .unwrap()
        .changes
        .len(),
        0
    );
    for busy in ["member5", "translator"] {
        let e = c.apply(&Cmd::StartListen { mix: mix(busy) }).unwrap_err();
        assert_eq!(e.code, ErrCode::NoSource);
    }
    let out = c
        .apply(&Cmd::StopListen {
            mix: mix("member4"),
        })
        .unwrap();
    assert_eq!(out.rt, vec![RtOp::Listen { slot: 1, mix: None }]);
    assert_eq!(c.transient().listen, [Some(mix("engineer")), None]);
    assert_eq!(
        c.apply(&Cmd::StopListen {
            mix: mix("member4")
        })
        .unwrap()
        .rt
        .len(),
        0
    );
    c.apply(&Cmd::StartListen {
        mix: mix("translator"),
    })
    .unwrap();
    c.apply(&Cmd::StopListen {
        mix: mix("engineer"),
    })
    .unwrap();
    assert_eq!(c.transient().listen, [None, Some(mix("translator"))]);
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
