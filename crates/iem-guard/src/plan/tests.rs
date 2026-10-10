//! The planner and its error policy (`plan.rs`, `plan/policy.rs`), every
//! combination of facts; the fixtures the activation tests share.

use super::*;

fn at(p: &[Step], s: Step) -> Option<usize> {
    p.iter().position(|x| *x == s)
}

fn has(p: &[Step], s: Step) -> bool {
    at(p, s).is_some()
}

fn every(mut check: impl FnMut(Facts, Vec<Step>)) {
    for bits in 0..(1u32 << FACT_BITS) {
        let f = Facts::from_bits(bits);
        check(f, plan(Mode::Event, &f));
    }
}

fn every_entry(mut check: impl FnMut(Mode, Facts, Vec<Step>)) {
    for bits in 0..(1u32 << FACT_BITS) {
        let f = Facts::from_bits(bits);
        for to in [Mode::Dev, Mode::Live] {
            check(to, f, plan(to, &f));
        }
    }
}

pub(super) fn band_up() -> Facts {
    Facts {
        reaper: true,
        app: true,
        reaper_holds_module: true,
        app_serves: true,
        ..Facts::default()
    }
}

pub(super) fn iemmixer_up() -> Facts {
    Facts {
        engine: true,
        server: true,
        tray: true,
        runner: true,
        ..Facts::default()
    }
}

#[test]
fn from_bits_sets_each_field_from_its_own_bit() {
    assert_eq!(Facts::from_bits(0), Facts::default());
    let all = Facts::from_bits((1 << FACT_BITS) - 1);
    assert_eq!(
        all,
        Facts {
            reaper: true,
            app: true,
            engine: true,
            server: true,
            tray: true,
            runner: true,
            reaper_holds_module: true,
            app_serves: true,
            other_module_holder: true,
        }
    );
    let one = |n: u32| Facts::from_bits(1 << n);
    assert_eq!(
        one(0),
        Facts {
            reaper: true,
            ..Facts::default()
        }
    );
    assert_eq!(
        one(1),
        Facts {
            app: true,
            ..Facts::default()
        }
    );
    assert_eq!(
        one(2),
        Facts {
            engine: true,
            ..Facts::default()
        }
    );
    assert_eq!(
        one(3),
        Facts {
            server: true,
            ..Facts::default()
        }
    );
    assert_eq!(
        one(4),
        Facts {
            tray: true,
            ..Facts::default()
        }
    );
    assert_eq!(
        one(5),
        Facts {
            runner: true,
            ..Facts::default()
        }
    );
    assert_eq!(
        one(6),
        Facts {
            reaper_holds_module: true,
            ..Facts::default()
        }
    );
    assert_eq!(
        one(7),
        Facts {
            app_serves: true,
            ..Facts::default()
        }
    );
    assert_eq!(
        one(8),
        Facts {
            other_module_holder: true,
            ..Facts::default()
        }
    );
    // Bits above the facts change nothing.
    assert_eq!(Facts::from_bits(1 << FACT_BITS), Facts::default());
}

#[test]
fn modes_and_steps_serialise_in_snake_case() {
    assert_eq!(Mode::default(), Mode::Event);
    assert_eq!(serde_json::to_string(&Mode::Live).unwrap(), r#""live""#);
    assert_eq!(
        serde_json::to_string(&Step::ReaperSaveQuit).unwrap(),
        r#""reaper_save_quit""#
    );
    assert_eq!(
        serde_json::to_string(&PrefFail::StartReaperWithAlarm).unwrap(),
        r#""start_reaper_with_alarm""#
    );
    assert_eq!(
        serde_json::to_string(&Health::Parked).unwrap(),
        r#""parked""#
    );
    for s in Step::ALL {
        let text = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Step>(&text).unwrap(), s);
    }
    // ALL lists every step exactly once.
    for (i, s) in Step::ALL.iter().enumerate() {
        assert_eq!(at(&Step::ALL, *s), Some(i), "{s:?}");
    }
}

#[test]
fn every_event_plan_checks_the_preference_before_reaper() {
    every(|f, p| {
        let pref = at(&p, Step::PrefCheck).expect("PrefCheck in every event plan");
        for s in [Step::ReaperStart, Step::ReaperHandover, Step::AppStart] {
            if let Some(i) = at(&p, s) {
                assert!(pref < i, "{f:?}: {s:?} before PrefCheck");
            }
        }
        if let Some(e) = at(&p, Step::EngineStop) {
            assert!(e < pref, "{f:?}: EngineStop after PrefCheck");
        }
        // Every other holder of the driver module has left: the check
        // never writes while a process has the driver open (#9
        // 2026-09-28), so it must not find one it could have waited for.
        if let Some(h) = at(&p, Step::HolderGone) {
            assert!(h < pref, "{f:?}: HolderGone after PrefCheck");
        }
    });
}

#[test]
fn every_event_plan_ends_with_the_fingerprint() {
    every(|_, p| assert_eq!(p.last(), Some(&Step::Fingerprint)));
}

#[test]
fn every_event_plan_starts_what_does_not_serve_and_keeps_what_does() {
    every(|f, p| {
        let why = format!("{f:?}: {p:?}");
        let reaper_ok = f.reaper && f.reaper_holds_module;
        let app_ok = f.app && f.app_serves;
        assert_eq!(has(&p, Step::ReaperStart), !reaper_ok, "{why}");
        assert_eq!(
            has(&p, Step::ReaperSaveQuit),
            f.reaper && !f.reaper_holds_module,
            "{why}"
        );
        assert_eq!(has(&p, Step::AppStart), !app_ok, "{why}");
        assert_eq!(has(&p, Step::AppStop), f.app && !f.app_serves, "{why}");
        assert_eq!(has(&p, Step::HolderGone), f.other_module_holder, "{why}");
        assert_eq!(has(&p, Step::EngineStop), f.engine, "{why}");
        assert_eq!(has(&p, Step::ServerStop), f.server, "{why}");
        assert_eq!(has(&p, Step::TrayStop), f.tray, "{why}");
        assert_eq!(has(&p, Step::JobsCancel), f.runner, "{why}");
        assert_eq!(has(&p, Step::RunnerStop), f.runner, "{why}");
        for s in [
            Step::TuningExit,
            Step::PrefCheck,
            Step::ReaperHandover,
            Step::AppHandover,
            Step::Fingerprint,
        ] {
            assert!(has(&p, s), "{why}: {s:?} missing");
        }
        // Never planned: the entry steps and the runner's own EngineHealth.
        for s in [
            Step::Precheck,
            Step::TuningEnter,
            Step::Data,
            Step::EngineStart,
            Step::EngineArm,
            Step::ServerStart,
            Step::TrayStart,
            Step::IdentityCheck,
            Step::RunnerStart,
            Step::EngineHealth,
        ] {
            assert!(!has(&p, s), "{why}: {s:?} planned");
        }
        // Order: a restart quits before it starts, a foreign holder leaves
        // before REAPER is touched, REAPER is handed over before the app.
        let before = |a: Step, b: Step| {
            if let (Some(i), Some(j)) = (at(&p, a), at(&p, b)) {
                assert!(i < j, "{why}: {a:?} after {b:?}");
            }
        };
        before(Step::JobsCancel, Step::RunnerStop);
        before(Step::RunnerStop, Step::EngineStop);
        before(Step::EngineStop, Step::ServerStop);
        before(Step::ServerStop, Step::TrayStop);
        before(Step::TrayStop, Step::TuningExit);
        before(Step::TuningExit, Step::HolderGone);
        before(Step::TuningExit, Step::PrefCheck);
        before(Step::HolderGone, Step::PrefCheck);
        before(Step::HolderGone, Step::ReaperSaveQuit);
        before(Step::HolderGone, Step::ReaperStart);
        before(Step::ReaperSaveQuit, Step::ReaperStart);
        before(Step::ReaperStart, Step::ReaperHandover);
        before(Step::ReaperHandover, Step::AppStop);
        before(Step::ReaperHandover, Step::AppStart);
        before(Step::AppStop, Step::AppStart);
        before(Step::AppStart, Step::AppHandover);
    });
}

#[test]
fn failed_engine_stop_never_starts_the_app_without_reaper() {
    for pf in [PrefFail::StartReaperWithAlarm, PrefFail::KeepReaperDown] {
        for h in [
            None,
            Some(Health::Healthy),
            Some(Health::Dead),
            Some(Health::Parked),
        ] {
            let e = on_error(Mode::Event, Step::EngineStop, h, pf);
            assert!(
                matches!(e, OnError::KeepServing | OnError::StopAskOwner),
                "{h:?}: {e:?}"
            );
        }
    }
    assert_eq!(
        on_error(
            Mode::Event,
            Step::EngineStop,
            Some(Health::Healthy),
            PrefFail::KeepReaperDown
        ),
        OnError::KeepServing
    );
}

#[test]
fn a_failed_release_keeps_serving_only_while_the_engine_is_healthy() {
    for step in [Step::EngineStop, Step::EngineHealth] {
        for pf in [PrefFail::StartReaperWithAlarm, PrefFail::KeepReaperDown] {
            assert_eq!(
                on_error(Mode::Event, step, Some(Health::Healthy), pf),
                OnError::KeepServing
            );
            assert_eq!(
                on_error(Mode::Event, step, Some(Health::Dead), pf),
                OnError::StopAskOwner
            );
            assert_eq!(
                on_error(Mode::Event, step, Some(Health::Parked), pf),
                OnError::StopAskOwner
            );
            assert_eq!(on_error(Mode::Event, step, None, pf), OnError::StopAskOwner);
        }
    }
}

/// #38 (owner, 2026-10-06): a dev or live entry is the precheck, the
/// stops and the start, from any mode and with any facts. No step reads
/// the stage: only the owner's signal decides whether the PC may change,
/// and other devices on the Dante network feed the card's inputs.
#[test]
fn every_entry_is_the_precheck_the_stops_and_the_start() {
    every_entry(|to, f, p| {
        let mut want = vec![Step::Precheck];
        if f.app {
            want.push(Step::AppStop);
        }
        if f.reaper {
            want.push(Step::ReaperSaveQuit);
        }
        if f.runner {
            want.extend([Step::JobsCancel, Step::RunnerStop]);
        }
        if f.engine {
            want.push(Step::EngineStop);
        }
        if f.server {
            want.push(Step::ServerStop);
        }
        if f.tray {
            want.push(Step::TrayStop);
        }
        want.extend([
            Step::TuningEnter,
            Step::Data,
            Step::PrefCheck,
            Step::EngineStart,
            Step::EngineArm,
            Step::ServerStart,
            Step::TrayStart,
            Step::IdentityCheck,
        ]);
        if to == Mode::Dev {
            want.push(Step::RunnerStart);
        }
        assert_eq!(p, want, "{to:?} {f:?}");
    });
}

#[test]
fn the_app_stops_before_reaper_saves() {
    let f = band_up();
    let p = plan(Mode::Dev, &f);
    let (a, r) = (
        at(&p, Step::AppStop).unwrap(),
        at(&p, Step::ReaperSaveQuit).unwrap(),
    );
    assert!(a < r, "{p:?}");
}

#[test]
fn event_to_dev_is_the_whole_entry_in_order() {
    assert_eq!(
        plan(Mode::Dev, &band_up()),
        [
            Step::Precheck,
            Step::AppStop,
            Step::ReaperSaveQuit,
            Step::TuningEnter,
            Step::Data,
            Step::PrefCheck,
            Step::EngineStart,
            Step::EngineArm,
            Step::ServerStart,
            Step::TrayStart,
            Step::IdentityCheck,
            Step::RunnerStart,
        ]
    );
}

/// An engine that ended while it held the card (a hard kill, a power
/// loss) left 32, and a new engine refuses the card unless it finds
/// REAPER's original (#9 2026-09-28). So every entry restores the
/// preference right before the engine starts: once, after REAPER quit
/// and after our own engine stopped (no open driver sees the write),
/// never while REAPER may hold the card.
#[test]
fn every_entry_restores_the_preference_right_before_the_engine_starts() {
    every_entry(|to, f, p| {
        let why = format!("{to:?} {f:?}: {p:?}");
        let pref = at(&p, Step::PrefCheck).expect("PrefCheck in every entry");
        assert_eq!(
            p.iter().filter(|s| **s == Step::PrefCheck).count(),
            1,
            "{why}"
        );
        assert_eq!(at(&p, Step::EngineStart), Some(pref + 1), "{why}");
        for s in [
            Step::AppStop,
            Step::ReaperSaveQuit,
            Step::EngineStop,
            Step::TuningEnter,
            Step::Data,
        ] {
            if let Some(i) = at(&p, s) {
                assert!(i < pref, "{why}: {s:?} after PrefCheck");
            }
        }
    });
    assert_eq!(
        plan(Mode::Live, &band_up()),
        [
            Step::Precheck,
            Step::AppStop,
            Step::ReaperSaveQuit,
            Step::TuningEnter,
            Step::Data,
            Step::PrefCheck,
            Step::EngineStart,
            Step::EngineArm,
            Step::ServerStart,
            Step::TrayStart,
            Step::IdentityCheck,
        ]
    );
    // Our own engine stops (and restores the preference as it releases
    // the card) before the check.
    assert_eq!(
        plan(Mode::Dev, &iemmixer_up()),
        [
            Step::Precheck,
            Step::JobsCancel,
            Step::RunnerStop,
            Step::EngineStop,
            Step::ServerStop,
            Step::TrayStop,
            Step::TuningEnter,
            Step::Data,
            Step::PrefCheck,
            Step::EngineStart,
            Step::EngineArm,
            Step::ServerStart,
            Step::TrayStart,
            Step::IdentityCheck,
            Step::RunnerStart,
        ]
    );
}

/// A restore that fails before the engine starts unwinds to event like
/// any entry step; the event plan's own `PrefCheck` then follows
/// `on_pref_fail` as before.
#[test]
fn a_failed_pref_check_in_an_entry_unwinds_to_event() {
    for to in [Mode::Dev, Mode::Live] {
        for pf in [PrefFail::StartReaperWithAlarm, PrefFail::KeepReaperDown] {
            assert_eq!(
                on_error(to, Step::PrefCheck, None, pf),
                OnError::Unwind,
                "{to:?} {pf:?}"
            );
        }
    }
    let back = plan(Mode::Event, &Facts::default());
    assert!(has(&back, Step::PrefCheck), "{back:?}");
}

#[test]
fn dev_to_event_is_the_whole_teardown_in_order() {
    assert_eq!(
        plan(Mode::Event, &iemmixer_up()),
        [
            Step::JobsCancel,
            Step::RunnerStop,
            Step::EngineStop,
            Step::ServerStop,
            Step::TrayStop,
            Step::TuningExit,
            Step::PrefCheck,
            Step::ReaperStart,
            Step::ReaperHandover,
            Step::AppStart,
            Step::AppHandover,
            Step::Fingerprint,
        ]
    );
}

#[test]
fn every_entry_plan_has_its_shape() {
    every_entry(|to, f, p| {
        let why = format!("{to:?} {f:?}: {p:?}");
        assert_eq!(p.first(), Some(&Step::Precheck), "{why}");
        assert_eq!(has(&p, Step::AppStop), f.app, "{why}");
        assert_eq!(has(&p, Step::ReaperSaveQuit), f.reaper, "{why}");
        assert_eq!(has(&p, Step::EngineStop), f.engine, "{why}");
        assert_eq!(has(&p, Step::ServerStop), f.server, "{why}");
        assert_eq!(has(&p, Step::TrayStop), f.tray, "{why}");
        assert_eq!(has(&p, Step::JobsCancel), f.runner, "{why}");
        assert_eq!(has(&p, Step::RunnerStop), f.runner, "{why}");
        assert_eq!(has(&p, Step::RunnerStart), to == Mode::Dev, "{why}");
        assert_eq!(
            p.last(),
            Some(if to == Mode::Dev {
                &Step::RunnerStart
            } else {
                &Step::IdentityCheck
            }),
            "{why}"
        );
        // Nothing of the band's system is started, and no event-only step
        // runs (`PrefCheck` runs in both: right before the engine here).
        for s in [
            Step::ReaperStart,
            Step::ReaperHandover,
            Step::AppStart,
            Step::AppHandover,
            Step::TuningExit,
            Step::HolderGone,
            Step::Fingerprint,
            Step::EngineHealth,
        ] {
            assert!(!has(&p, s), "{why}: {s:?} planned");
        }
        // The band's system is down and ours is stopped before tuning.
        let tuning = at(&p, Step::TuningEnter).expect("TuningEnter");
        for s in [
            Step::AppStop,
            Step::ReaperSaveQuit,
            Step::JobsCancel,
            Step::RunnerStop,
            Step::EngineStop,
            Step::ServerStop,
            Step::TrayStop,
        ] {
            if let Some(i) = at(&p, s) {
                assert!(i < tuning, "{why}: {s:?} after TuningEnter");
            }
        }
        if let (Some(a), Some(r)) = (at(&p, Step::AppStop), at(&p, Step::ReaperSaveQuit)) {
            assert!(a < r, "{why}: the app stops before REAPER saves");
        }
    });
}

#[test]
fn the_runner_starts_only_in_dev() {
    for f in [Facts::default(), band_up(), iemmixer_up()] {
        let dev = plan(Mode::Dev, &f);
        assert_eq!(dev.last(), Some(&Step::RunnerStart), "{f:?}");
        assert!(!has(&plan(Mode::Live, &f), Step::RunnerStart), "{f:?}");
        assert!(!has(&plan(Mode::Event, &f), Step::RunnerStart), "{f:?}");
    }
}

#[test]
fn event_in_event_only_checks() {
    assert_eq!(
        plan(Mode::Event, &band_up()),
        [
            Step::TuningExit,
            Step::PrefCheck,
            Step::ReaperHandover,
            Step::AppHandover,
            Step::Fingerprint,
        ]
    );
}

#[test]
fn a_stale_reaper_is_restarted() {
    let f = Facts {
        reaper_holds_module: false,
        ..band_up()
    };
    let p = plan(Mode::Event, &f);
    let pref = at(&p, Step::PrefCheck).unwrap();
    let quit = at(&p, Step::ReaperSaveQuit).expect("ReaperSaveQuit");
    let start = at(&p, Step::ReaperStart).expect("ReaperStart");
    assert!(pref < quit && quit < start, "{p:?}");
    assert!(
        !has(&p, Step::AppStart),
        "the serving app is left alone: {p:?}"
    );
}

#[test]
fn an_app_that_does_not_serve_is_restarted() {
    let f = Facts {
        app_serves: false,
        ..band_up()
    };
    let p = plan(Mode::Event, &f);
    let stop = at(&p, Step::AppStop).expect("AppStop");
    let start = at(&p, Step::AppStart).expect("AppStart");
    assert!(stop < start, "{p:?}");
    assert!(
        !has(&p, Step::ReaperStart),
        "the serving REAPER is left alone: {p:?}"
    );
}

#[test]
fn another_holder_blocks_reaper() {
    let f = Facts {
        other_module_holder: true,
        ..Facts::default()
    };
    let p = plan(Mode::Event, &f);
    let gone = at(&p, Step::HolderGone).expect("HolderGone");
    assert!(gone < at(&p, Step::ReaperStart).unwrap(), "{p:?}");
    // The holder leaves before the preference is checked: the check
    // never writes while a process has the driver open (#9 2026-09-28).
    assert_eq!(
        p,
        [
            Step::TuningExit,
            Step::HolderGone,
            Step::PrefCheck,
            Step::ReaperStart,
            Step::ReaperHandover,
            Step::AppStart,
            Step::AppHandover,
            Step::Fingerprint,
        ]
    );
    for pf in [PrefFail::StartReaperWithAlarm, PrefFail::KeepReaperDown] {
        assert_eq!(
            on_error(Mode::Event, Step::HolderGone, None, pf),
            OnError::StopAskOwner
        );
    }
}

#[test]
fn a_failed_pref_check_follows_the_choice() {
    assert_eq!(
        on_error(
            Mode::Event,
            Step::PrefCheck,
            None,
            PrefFail::StartReaperWithAlarm
        ),
        OnError::Continue
    );
    assert_eq!(
        on_error(Mode::Event, Step::PrefCheck, None, PrefFail::KeepReaperDown),
        OnError::StopAskOwner
    );
}

/// A failed app stop skips the app start (the old app may still run),
/// and since #10 it asks the owner: the event plan stops only an app
/// that does not serve, so after the failure none serves and the phones
/// cannot change mixes (coordinator's decision, 2026-10-08).
#[test]
fn a_failed_app_stop_skips_the_app_start_and_asks_the_owner() {
    for pf in [PrefFail::StartReaperWithAlarm, PrefFail::KeepReaperDown] {
        assert_eq!(
            on_error(Mode::Event, Step::AppStop, None, pf),
            OnError::SkipAskOwner(&[Step::AppStart])
        );
    }
}

/// #10 (coordinator's decision, 2026-10-08): an event switch that ends
/// without the predecessor app serving is not done. A failed app
/// handover asks the owner and the plan goes on (the fingerprint);
/// REAPER keeps playing the band's mixes.
#[test]
fn a_failed_app_handover_asks_the_owner_and_goes_on() {
    for pf in [PrefFail::StartReaperWithAlarm, PrefFail::KeepReaperDown] {
        for h in [None, Some(Health::Dead)] {
            assert_eq!(
                on_error(Mode::Event, Step::AppHandover, h, pf),
                OnError::ContinueAskOwner,
                "{pf:?} {h:?}"
            );
        }
    }
}

#[test]
fn a_failed_reaper_quit_or_start_asks_the_owner() {
    for s in [Step::ReaperSaveQuit, Step::ReaperStart] {
        for pf in [PrefFail::StartReaperWithAlarm, PrefFail::KeepReaperDown] {
            assert_eq!(
                on_error(Mode::Event, s, None, pf),
                OnError::StopAskOwner,
                "{s:?}"
            );
        }
    }
}

/// #10 (2026-10-08): an event switch whose REAPER handover failed
/// (REAPER could not be made to run, or a check failed) never ends
/// `done`: the owner's prepared question, and the plan goes on to the
/// app as before (the band keeps what works). Into dev or live it
/// unwinds like every other step.
#[test]
fn a_failed_reaper_handover_asks_the_owner_and_goes_on() {
    for pf in [PrefFail::StartReaperWithAlarm, PrefFail::KeepReaperDown] {
        for h in [None, Some(Health::Dead), Some(Health::Healthy)] {
            assert_eq!(
                on_error(Mode::Event, Step::ReaperHandover, h, pf),
                OnError::ContinueAskOwner,
                "{pf:?} {h:?}"
            );
        }
        for to in [Mode::Dev, Mode::Live] {
            assert_eq!(
                on_error(to, Step::ReaperHandover, None, pf),
                OnError::Unwind
            );
        }
    }
}

#[test]
fn other_event_failures_alarm_and_go_on() {
    let special = [
        Step::EngineStop,
        Step::EngineHealth,
        Step::PrefCheck,
        Step::HolderGone,
        Step::ReaperSaveQuit,
        Step::ReaperStart,
        Step::ReaperHandover,
        Step::AppStop,
        Step::AppHandover,
    ];
    for s in Step::ALL {
        if special.contains(&s) {
            continue;
        }
        for pf in [PrefFail::StartReaperWithAlarm, PrefFail::KeepReaperDown] {
            assert_eq!(
                on_error(Mode::Event, s, None, pf),
                OnError::Continue,
                "{s:?}"
            );
            assert_eq!(
                on_error(Mode::Event, s, Some(Health::Dead), pf),
                OnError::Continue,
                "{s:?}"
            );
        }
    }
}

#[test]
fn every_dev_or_live_error_unwinds() {
    for to in [Mode::Dev, Mode::Live] {
        for s in Step::ALL {
            for h in [
                None,
                Some(Health::Healthy),
                Some(Health::Dead),
                Some(Health::Parked),
            ] {
                for pf in [PrefFail::StartReaperWithAlarm, PrefFail::KeepReaperDown] {
                    assert_eq!(
                        on_error(to, s, h, pf),
                        OnError::Unwind,
                        "{to:?} {s:?} {h:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn a_failed_dev_switch_unwinds_to_event() {
    // The facts after a failure at EngineStart: REAPER and the app were
    // quit, the engine never came up.
    let entry = plan(Mode::Dev, &band_up());
    assert!(has(&entry, Step::EngineStart));
    let p = plan(Mode::Event, &Facts::default());
    let (reaper, app) = (
        at(&p, Step::ReaperStart).expect("REAPER starts"),
        at(&p, Step::AppStart).expect("the app starts"),
    );
    assert!(reaper < app, "{p:?}");
    assert!(at(&p, Step::ReaperHandover).unwrap() < app, "{p:?}");
    assert_eq!(p.last(), Some(&Step::Fingerprint));
}
