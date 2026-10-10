//! The test signal (X13), the HIL signal and its mask (S6), `Arm`, fault
//! injection and HIL's forced reopen.

use super::*;

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

/// A core on the test site whose engine opened HIL's spare outputs: the
/// test site's `[guard] hil_tx` (TX 94, 95), as `run` does under the
/// test-signal flag.
fn hil_core(flags: Flags) -> Core {
    core(flags).with_hil(vec![94, 95])
}

const TEST_FLAG: Flags = Flags {
    test_signal: true,
    fault_injection: false,
};

#[test]
fn the_hil_test_signal_needs_the_flag_stays_under_the_cap_and_names_spare_outputs() {
    let hil = |dbfs: f64, card_tx: Vec<u16>| Cmd::HilTestSignal {
        input: input("mic3"),
        hz: 1000.0,
        dbfs,
        ttl_s: 0.5,
        card_tx,
        listen: false,
    };
    let mut off = hil_core(Flags::default());
    assert_eq!(code(off.apply(&hil(-30.0, vec![94]))), ErrCode::Forbidden);
    assert_eq!(code(off.apply(&hil(-10.0, vec![]))), ErrCode::Forbidden);
    let mut c = hil_core(TEST_FLAG);
    assert_eq!(c.hil(), [94, 95]);
    assert!(core(TEST_FLAG).hil().is_empty(), "none unless opened");
    // Above the −20 dBFS cap it is refused, never lowered; the cap itself is fine.
    assert_eq!(code(c.apply(&hil(-19.9, vec![94]))), ErrCode::BadValue);
    assert_eq!(code(c.apply(&hil(f64::NAN, vec![94]))), ErrCode::BadValue);
    assert_eq!(code(c.apply(&hil(-30.0, vec![]))), ErrCode::BadValue);
    // A card output the engine did not open for HIL is unknown.
    let unknown = c.apply(&hil(-30.0, vec![94, 96])).unwrap_err();
    assert_eq!(
        (unknown.code, unknown.msg.as_str()),
        (
            ErrCode::UnknownId,
            "card output 96 is not a HIL output of this engine ([guard] hil_tx)"
        )
    );
    assert_eq!(
        code(c.apply(&hil(-30.0, vec![101]))),
        ErrCode::UnknownId,
        "an RX"
    );
    assert!(
        c.transient().test_signal.is_none(),
        "a refusal changes nothing"
    );
    assert_eq!(c.rev(), 0);
    let out = c.apply(&hil(-20.0, vec![95, 94, 95])).unwrap();
    assert_eq!(out.rev, 1);
    let RtOp::HilTestSignal {
        i,
        hz,
        amp,
        ttl,
        mask,
        listen,
    } = out.rt[0]
    else {
        panic!("{:?}", out.rt)
    };
    assert_eq!((i, hz, ttl, listen), (2, 1000.0, 48_000, false));
    assert!((amp - 0.1).abs() < 1e-15, "{amp}");
    // HIL slots in the order the engine opened its spare outputs.
    let on: Vec<usize> = (0..MAX_HIL).filter(|k| mask[*k]).collect();
    assert_eq!(on, vec![0, 1]);
    let t = c.transient().test_signal.unwrap();
    assert_eq!((t.input, t.dbfs, t.ttl_s), (input("mic3"), -20.0, 0.5));
    // It is the test signal: StopTestSignal ends it like any other.
    let stop = c.apply(&Cmd::StopTestSignal).unwrap();
    assert_eq!(stop.rt, vec![RtOp::StopTestSignal]);
    let out = c.apply(&hil(-30.0, vec![95])).unwrap();
    let RtOp::HilTestSignal { mask, .. } = out.rt[0] else {
        panic!("{:?}", out.rt)
    };
    let on: Vec<usize> = (0..MAX_HIL).filter(|k| mask[*k]).collect();
    assert_eq!(on, vec![1]);
    c.apply(&Cmd::StopTestSignal).unwrap();
    // The plain test signal carries no mask.
    let plain = c
        .apply(&Cmd::StartTestSignal {
            input: input("mic3"),
            hz: 1000.0,
            dbfs: -30.0,
            ttl_s: 0.5,
        })
        .unwrap();
    assert!(matches!(plain.rt[0], RtOp::TestSignal { i: 2, .. }));
}

/// The HIL signal goes only to spare card outputs, never to a channel a
/// band member hears (the owner's decision on #9 of 2026-09-28): a mix's
/// TX is refused wherever it stands in the list, whatever the engine
/// opened (even a list that wrongly holds one), and nothing starts.
#[test]
fn a_hil_signal_never_names_a_mixs_tx() {
    for mut c in [
        core(TEST_FLAG),
        hil_core(TEST_FLAG),
        core(TEST_FLAG).with_hil(vec![94, 72]),
    ] {
        for (tx, mix) in [
            (72, "member1"),
            (71, "member1"),
            (88, "member9"),
            (91, "engineer"),
            (93, "translator"),
        ] {
            for card_tx in [vec![tx, 94], vec![94, tx]] {
                let e = c
                    .apply(&Cmd::HilTestSignal {
                        input: input("mic3"),
                        hz: 1000.0,
                        dbfs: -30.0,
                        ttl_s: 0.5,
                        card_tx: card_tx.clone(),
                        listen: false,
                    })
                    .unwrap_err();
                if card_tx[0] == 94 && c.hil().is_empty() {
                    // 94 is unknown to an engine without HIL outputs.
                    assert_eq!(e.code, ErrCode::UnknownId, "{card_tx:?}");
                    continue;
                }
                assert_eq!(e.code, ErrCode::Forbidden, "{card_tx:?}");
                assert_eq!(
                    e.msg,
                    format!(
                        "card output {tx} is mix {mix}'s TX: the HIL signal goes only to spare outputs"
                    )
                );
            }
        }
        assert!(c.transient().test_signal.is_none());
        assert_eq!(c.rev(), 0);
    }
}

/// While a HIL test signal runs (its TTL, S6 design note §4) a plain test
/// signal may not replace it: that would lift the card mask and sound on
/// every TX that hears the input. A stop ends it as usual, and after its end
/// a plain one starts again (lane A review).
#[test]
fn a_plain_test_signal_never_lifts_a_running_hil_mask() {
    let mut c = hil_core(TEST_FLAG);
    let hil = Cmd::HilTestSignal {
        input: input("mic3"),
        hz: 1000.0,
        dbfs: -30.0,
        ttl_s: 0.5,
        card_tx: vec![94],
        listen: false,
    };
    let plain = Cmd::StartTestSignal {
        input: input("mic2"),
        hz: 500.0,
        dbfs: -30.0,
        ttl_s: 0.5,
    };
    c.apply(&hil).unwrap();
    assert_eq!(code(c.apply(&plain)), ErrCode::Forbidden);
    assert_eq!(
        c.transient().test_signal.map(|t| t.input),
        Some(input("mic3"))
    );
    // The end of its TTL (the control loop's) frees it.
    c.end_test_signal();
    assert!(c.apply(&plain).is_ok());
    // A HIL signal replaces a plain one; a stop frees it too.
    c.apply(&hil).unwrap();
    c.apply(&Cmd::StopTestSignal).unwrap();
    assert!(c.apply(&plain).is_ok());
}

#[test]
fn a_hil_output_beyond_the_mask_is_refused() {
    // An engine given more spare outputs than a mask holds (check-site and
    // run refuse such a site): the last slot is taken, one more is refused.
    let spare: Vec<u16> = (94..=94 + MAX_HIL as u16).collect();
    assert_eq!(spare.len(), MAX_HIL + 1);
    let mut c = core(TEST_FLAG).with_hil(spare.clone());
    let hil = |ch: u16| Cmd::HilTestSignal {
        input: input("mic3"),
        hz: 1000.0,
        dbfs: -30.0,
        ttl_s: 0.5,
        card_tx: vec![ch],
        listen: false,
    };
    let out = c.apply(&hil(spare[MAX_HIL - 1])).unwrap();
    let RtOp::HilTestSignal { mask, .. } = out.rt[0] else {
        panic!("{:?}", out.rt)
    };
    assert!(mask[MAX_HIL - 1]);
    assert_eq!(mask.iter().filter(|b| **b).count(), 1);
    let e = c.apply(&hil(spare[MAX_HIL])).unwrap_err();
    assert_eq!(
        (e.code, e.msg.as_str()),
        (
            ErrCode::BadValue,
            "card output 102 is beyond the first 8 HIL outputs"
        )
    );
}

#[test]
fn arm_reaches_the_processor_without_a_revision() {
    let mut c = core(Flags::default());
    let out = c.apply(&Cmd::Arm).unwrap();
    assert_eq!(
        (out.rev, out.rt, out.changes.len(), out.effect),
        (0, vec![RtOp::Arm], 0, Effect::None)
    );
    assert_eq!(
        code(c.apply(&Cmd::Batch {
            ops: vec![Cmd::Arm]
        })),
        ErrCode::BadRequest,
        "not a state edit"
    );
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
fn seh_injection_needs_the_flag() {
    // The owner-approved SEH test (design §10): a structured exception on the
    // RT thread, only under the fault-injection flag, like InjectFault.
    let mut off = core(Flags::default());
    assert_eq!(code(off.apply(&Cmd::InjectSeh)), ErrCode::Forbidden);
    let mut on = core(Flags {
        test_signal: false,
        fault_injection: true,
    });
    let out = on.apply(&Cmd::InjectSeh).unwrap();
    assert_eq!((out.rev, out.rt), (0, vec![RtOp::Seh]));
}

#[test]
fn park_injection_needs_the_flag() {
    // The parked-engine test (design §10 test #2, #35): the SEH test's
    // exception under the backend's hold, only under the fault-injection
    // flag; neither state nor revision changes.
    let mut off = core(Flags::default());
    assert_eq!(code(off.apply(&Cmd::InjectPark)), ErrCode::Forbidden);
    let mut test_signal_only = core(Flags {
        test_signal: true,
        fault_injection: false,
    });
    assert_eq!(
        code(test_signal_only.apply(&Cmd::InjectPark)),
        ErrCode::Forbidden
    );
    let mut on = core(Flags {
        test_signal: false,
        fault_injection: true,
    });
    let out = on.apply(&Cmd::InjectPark).unwrap();
    assert!(out.changes.is_empty());
    assert_eq!(out.effect, Effect::None);
    assert_eq!((out.rev, out.rt), (0, vec![RtOp::Park]));
}

#[test]
fn a_forced_reopen_needs_the_fault_flag_and_changes_nothing() {
    let mut off = core(Flags::default());
    assert_eq!(code(off.apply(&Cmd::ForceReopen)), ErrCode::Forbidden);
    let mut on = core(Flags {
        test_signal: false,
        fault_injection: true,
    });
    let out = on.apply(&Cmd::ForceReopen).unwrap();
    assert_eq!(out.effect, Effect::Reopen);
    assert_eq!((out.rev, out.rt, out.changes), (0, vec![], vec![]));
    assert_eq!(on.rev(), 0);
}
