//! The core's solo (X2) and listen slots (X3).

use super::*;

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
