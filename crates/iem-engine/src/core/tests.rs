use super::*;
use crate::test_support::test_site;
use iem_engine_proto::{DB_OFF, Eq};

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
        Cmd::ForceReopen,
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

mod solo_listen;
mod state;
mod test_signal;
