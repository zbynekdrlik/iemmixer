//! The switch runner against `FakePc` (`daemon/runner.rs`): a dev entry's
//! plan, the preference check before REAPER and before the engine, the
//! logon task's result, the unwind of a failed entry, the engine's
//! readiness, "ide event"'s pre-emption and the event plan's error policy.

use std::time::{Duration, Instant};

use super::tests::{
    HELD, INIT, SHA, ask, band_up, dev, iemmixer_up, preempt_after, status_reply, steps, texts,
};
use super::*;
use crate::crash;
use crate::effects::engine::{Ready, ReadyWindow};
use crate::effects::tuning::{Logon, LogonPref};
use crate::pc::fake::{Call, FakePc};
use crate::pc::{CardHolders, PrefHeld, Status};
use crate::plan::{Facts, Health};
use crate::proto::Request;

#[test]
fn the_parked_engines_alarm_asks_the_owner_and_names_the_health() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    pc.fail(Call::EngineStop, "no DriverReleased within 10 s");
    pc.health(Health::Parked);
    run_switch(&mut pc, &mut g, Mode::Dev, Mode::Event);
    let a = g.alarms.last().unwrap();
    assert_eq!(a.step, Some(Step::EngineStop));
    assert_eq!(
        a.text,
        "EngineStop: no DriverReleased within 10 s; health Some(Parked)"
    );
    assert!(a.owner_question);
    assert_eq!(g.state.mode, Mode::Event);
    assert_eq!(g.state.switching, None);
    // An unreadable health counts as dead.
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    pc.fail(Call::EngineStop, "refused");
    pc.fail(Call::EngineHealth, "no status");
    assert_eq!(
        run_switch(&mut pc, &mut g, Mode::Dev, Mode::Event),
        Outcome::NeedsOwner
    );
    assert_eq!(
        g.alarms.last().unwrap().text,
        "EngineStop: refused; health Some(Dead)"
    );
}

#[test]
fn the_kept_serving_alarm_says_so() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    pc.fail(Call::EngineStop, "no DriverReleased within 10 s");
    pc.health(Health::Healthy);
    let r = handle(
        &mut pc,
        &mut g,
        Request::Event {
            dry_run: false,
            signal: false,
        },
        INIT,
    );
    assert!(!r.ok);
    assert!(
        r.detail.starts_with(
            "event: the engine did not release; iemmixer keeps serving; the mode is dev"
        ),
        "{}",
        r.detail
    );
    assert_eq!(
        g.alarms.last().unwrap().text,
        "EngineStop: no DriverReleased within 10 s; engine healthy, iemmixer keeps serving"
    );
    // Its notice went out after the plan ended.
    assert_eq!(pc.calls().last(), Some(&Call::Notify));
    assert!(g.alarms.last().unwrap().notified);
}

#[test]
fn a_dev_entry_runs_the_whole_plan_and_serves() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    let r = handle(&mut pc, &mut g, dev(), INIT);
    assert!(r.ok, "{r:?}");
    assert!(
        r.detail
            .starts_with("dev: done; tuning enter: enter: ok; Dev data refreshed; "),
        "{}",
        r.detail
    );
    assert!(r.detail.contains("; Dev data refreshed; "), "{}", r.detail);
    assert!(
        r.detail
            .contains("engine ready: 32 frames, 30000 callbacks, 0 missed"),
        "{}",
        r.detail
    );
    assert_eq!(
        steps(&pc),
        [
            Call::Precheck,
            Call::AppStop,
            Call::ReaperSaveQuit,
            Call::Tuning,
            Call::Data,
            Call::PrefCheck,
            Call::EngineStart,
            Call::EngineReady,
            Call::EngineArm,
            Call::ServerStart,
            Call::TrayStart,
            Call::Identity,
            Call::RunnerStart,
            Call::TuningDrift,
        ]
    );
    assert_eq!(g.state.mode, Mode::Dev);
    assert_eq!(g.state.switching, None);
    assert!(g.alarms.all().is_empty());
    assert_eq!(pc.ready_secs, [READY_S]);
    assert_eq!(READY_S, 10);
    let v = g.shared.view();
    assert_eq!(
        (v.mode, v.running, v.last),
        (Mode::Dev, None, Some(Outcome::Done))
    );
    assert_eq!(v.epoch, 1);
    // A dev entry is no start's checks: it moves the fence (#42).
    assert_eq!(
        (v.fence, v.start_checks, v.began),
        (1, false, Some((Mode::Event, Mode::Dev)))
    );
}

/// An engine that ended while it held the card (a hard kill, a power loss)
/// left 32, and a new engine refuses the card unless it finds REAPER's
/// original (#9 2026-09-28): the dev entry restores it right before the
/// engine starts. A restore that fails unwinds to event like any entry
/// step, and the event plan's own check follows `on_pref_fail`.
#[test]
fn a_dev_entry_restores_the_preference_right_before_the_engine_starts() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    pc.pref_attempts = 1;
    let r = handle(&mut pc, &mut g, dev(), INIT);
    assert!(r.ok, "{r:?}");
    assert!(
        r.detail
            .contains("the preferred buffer was restored (1 writes)"),
        "{}",
        r.detail
    );
    let order = steps(&pc);
    let first = |c: Call| order.iter().position(|x| *x == c).unwrap();
    assert!(
        first(Call::ReaperSaveQuit) < first(Call::PrefCheck),
        "{order:?}"
    );
    assert_eq!(
        first(Call::PrefCheck) + 1,
        first(Call::EngineStart),
        "{order:?}"
    );
    assert_eq!(pc.count(Call::PrefCheck), 1);
    assert_eq!(g.state.mode, Mode::Dev);
    assert!(g.alarms.all().is_empty(), "{:?}", texts(&g));

    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    pc.fail(Call::PrefCheck, "3 restores failed");
    let r = handle(&mut pc, &mut g, dev(), INIT);
    assert!(!r.ok, "{r:?}");
    assert!(!pc.called(Call::EngineStart));
    // The entry's check, then the event plan's (REAPER with an alarm).
    assert_eq!(pc.count(Call::PrefCheck), 2);
    assert!(pc.called(Call::ReaperStart) && pc.called(Call::AppStart));
    assert_eq!(g.state.mode, Mode::Event);
    assert_eq!(
        g.alarms
            .iter()
            .filter(|a| a.step == Some(Step::PrefCheck))
            .count(),
        2,
        "{:?}",
        texts(&g)
    );
}

/// REAPER holds the card while the preference reads 32 (REAPER autostarted
/// at 32 after a power loss in dev time, #9 2026-09-28): the driver most
/// likely asks REAPER for a reset when the value changes while it is open,
/// a dropout mid-event. The event plan's check writes nothing: an alarm
/// names the value, REAPER is left alone and the switch is done (the band
/// keeps REAPER's sound), whatever `on_pref_fail` says.
#[test]
fn an_event_plan_never_writes_the_preference_under_a_reaper_that_holds_the_card() {
    for choice in [PrefFail::StartReaperWithAlarm, PrefFail::KeepReaperDown] {
        let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
        g.site.on_pref_fail = choice;
        g.state.pins.current = Some(SHA.into());
        pc.pref_attempts = 1;
        let r = ask(
            &mut pc,
            &mut g,
            Request::Event {
                dry_run: false,
                signal: false,
            },
        );
        assert!(r.ok, "{choice:?}: {r:?}");
        assert_eq!(
            r.detail,
            format!("event: done; tuning exit: exit: ok; {HELD}"),
            "{choice:?}"
        );
        assert_eq!(pc.pref_writes, 0, "{choice:?}");
        for c in [
            Call::ReaperSaveQuit,
            Call::ReaperStart,
            Call::AppStop,
            Call::AppStart,
        ] {
            assert!(!pc.called(c), "{choice:?}: {c:?}");
        }
        assert!(pc.called(Call::ReaperFacts) && pc.called(Call::AppAnswers));
        assert!(pc.called(Call::Fingerprint));
        assert_eq!(g.state.mode, Mode::Event);
        assert_eq!(texts(&g), [format!("PrefCheck: {HELD}")], "{choice:?}");
        let a = g.alarms.last().unwrap();
        assert_eq!(a.step, Some(Step::PrefCheck));
        assert!(!a.owner_question);
        assert_eq!(g.state.pref_held.as_deref(), Some(HELD));
        assert_eq!(
            status_reply(&g),
            format!("mode event; bundle {SHA}; {HELD}; 1 unacknowledged alarms")
        );
    }
}

/// The guard remembers a preference it left alone: a later check alarms no
/// more and still writes nothing, and the next plan that stops REAPER (the
/// dev entry's save and quit) restores it right before the engine starts.
#[test]
fn a_preference_left_under_reaper_is_restored_once_reaper_quits() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    pc.pref_attempts = 1;
    assert!(
        ask(
            &mut pc,
            &mut g,
            Request::Event {
                dry_run: false,
                signal: false,
            }
        )
        .ok
    );
    let r = ask(
        &mut pc,
        &mut g,
        Request::Event {
            dry_run: false,
            signal: false,
        },
    );
    assert!(r.ok, "{r:?}");
    assert_eq!(
        r.detail,
        format!("event: done; tuning exit: exit: ok; {HELD}")
    );
    assert_eq!(texts(&g), [format!("PrefCheck: {HELD}")]);
    assert_eq!(pc.pref_writes, 0);
    assert_eq!(g.state.pref_held.as_deref(), Some(HELD));

    let r = ask(&mut pc, &mut g, dev());
    assert!(r.ok, "{r:?}");
    assert!(
        r.detail
            .contains("the preferred buffer was restored (1 writes)"),
        "{}",
        r.detail
    );
    assert_eq!(pc.pref_writes, 1);
    assert!(pc.index(Call::ReaperSaveQuit) < pc.index(Call::EngineStart));
    assert_eq!(g.state.mode, Mode::Dev);
    assert_eq!(g.state.pref_held, None);
    assert!(!status_reply(&g).contains(HELD), "{}", status_reply(&g));
    assert_eq!(g.alarms.all().len(), 1, "{:?}", texts(&g));
}

/// "The same with no holder": the event plan's check restores as before,
/// with our engine stopped, with a REAPER that runs without the card, and
/// after a foreign holder (a spike window) has left.
#[test]
fn an_event_plan_restores_the_preference_when_nothing_holds_the_card() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    pc.pref_attempts = 1;
    let r = ask(
        &mut pc,
        &mut g,
        Request::Event {
            dry_run: false,
            signal: false,
        },
    );
    assert!(r.ok, "{r:?}");
    assert!(
        r.detail
            .contains("the preferred buffer was restored (1 writes)"),
        "{}",
        r.detail
    );
    assert_eq!(pc.pref_writes, 1);
    assert!(pc.index(Call::EngineStop) < pc.index(Call::PrefCheck));
    assert!(pc.index(Call::PrefCheck) < pc.index(Call::ReaperStart));
    assert!(g.alarms.all().is_empty(), "{:?}", texts(&g));
    assert_eq!(g.state.pref_held, None);

    let (mut pc, mut g) = (
        FakePc::new(Facts {
            reaper_holds_module: false,
            ..band_up()
        }),
        Guard::for_test(Mode::Event),
    );
    pc.pref_attempts = 1;
    assert!(
        ask(
            &mut pc,
            &mut g,
            Request::Event {
                dry_run: false,
                signal: false,
            }
        )
        .ok
    );
    assert_eq!(pc.pref_writes, 1);
    assert!(pc.index(Call::PrefCheck) < pc.index(Call::ReaperSaveQuit));
    assert!(pc.index(Call::ReaperSaveQuit) < pc.index(Call::ReaperStart));
    assert!(g.alarms.all().is_empty(), "{:?}", texts(&g));

    let (mut pc, mut g) = (
        FakePc::new(Facts {
            other_module_holder: true,
            ..Facts::default()
        }),
        Guard::for_test(Mode::Dev),
    );
    pc.pref_attempts = 1;
    let r = ask(
        &mut pc,
        &mut g,
        Request::Event {
            dry_run: false,
            signal: false,
        },
    );
    assert!(r.ok, "{r:?}");
    assert_eq!(pc.pref_writes, 1);
    assert!(pc.index(Call::HolderGone) < pc.index(Call::PrefCheck));
    assert!(pc.index(Call::PrefCheck) < pc.index(Call::ReaperStart));
    assert!(g.alarms.all().is_empty(), "{:?}", texts(&g));
}

/// Right before the engine starts a held driver fails the entry (the engine
/// would refuse the card) and nothing is written under the holder; the
/// event plan back lets the holder leave first, then restores.
#[test]
fn a_dev_entry_never_writes_the_preference_under_another_holder() {
    let (mut pc, mut g) = (
        FakePc::new(Facts {
            other_module_holder: true,
            ..Facts::default()
        }),
        Guard::for_test(Mode::Event),
    );
    g.state.pins.current = Some(SHA.into());
    pc.pref_attempts = 1;
    let r = ask(&mut pc, &mut g, dev());
    assert!(!r.ok, "{r:?}");
    assert!(!pc.called(Call::EngineStart));
    assert_eq!(g.state.mode, Mode::Event);
    assert_eq!(
        texts(&g),
        [
            "PrefCheck: the driver module is held by spike.exe (99) with the preferred buffer at \
          32; nothing was written"
        ]
    );
    // The event plan back: the holder leaves, the one write, then REAPER.
    assert_eq!(pc.count(Call::PrefCheck), 2);
    assert_eq!(pc.pref_writes, 1);
    assert!(pc.index(Call::HolderGone) < pc.index(Call::ReaperStart));
    assert_eq!(g.state.pref_held, None);
}

/// The logon task's run of `at` that left the preference at 32 under a
/// REAPER that holds the card.
fn logon_held(at: &str) -> Logon {
    Logon {
        at: at.into(),
        pref: LogonPref::Held(PrefHeld {
            value: Some("32".into()),
            by: CardHolders {
                reaper: true,
                names: "reaper.exe (11)".into(),
            },
        }),
    }
}

/// The elevated logon task (G1) never writes the preference under a holder
/// of the driver module either (#9 2026-09-28): what it left is in its
/// result, which the guard takes once per run (its `at`), at its start and
/// hourly: remembered, named in the status and alarmed once, with the text
/// of `PrefCheck`; a later run at the original drops it; a failed run
/// changes nothing.
#[test]
fn the_guard_takes_what_the_logon_task_left() {
    const RUN1: &str = "2026-09-28T06:00:00.0000000Z";
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    pc.logon = Some(logon_held(RUN1));
    assert_eq!(start(&mut pc, &mut g, 0), None);
    assert_eq!(texts(&g), [format!("logon task: {HELD}")]);
    assert!(!g.alarms.last().unwrap().owner_question);
    assert_eq!(g.state.pref_held.as_deref(), Some(HELD));
    assert_eq!(g.state.logon_seen.as_deref(), Some(RUN1));
    assert_eq!(
        status_text(&g),
        format!("mode event; no bundle; {HELD}; 1 unacknowledged alarms")
    );
    // The same run is not taken twice (a guard restart), also after a check
    // of the guard's own dropped what it remembered.
    g.state.pref_held = None;
    assert_eq!(start(&mut pc, &mut g, 0), None);
    assert_eq!(g.state.pref_held, None);
    assert_eq!(g.alarms.all().len(), 1);
    // A failed run is logged and changes nothing but the run seen.
    const RUN2: &str = "2026-09-29T06:00:00.0000000Z";
    g.state.pref_held = Some(HELD.into());
    pc.logon = Some(Logon {
        at: RUN2.into(),
        pref: LogonPref::Failed("the preference was not restored: it reads 32".into()),
    });
    assert_eq!(start(&mut pc, &mut g, 0), None);
    assert_eq!(g.state.pref_held.as_deref(), Some(HELD));
    assert_eq!(g.state.logon_seen.as_deref(), Some(RUN2));
    assert_eq!(g.alarms.all().len(), 1);
    // A run that found or restored the original drops it.
    pc.logon = Some(Logon {
        at: "2026-09-30T06:00:00.0000000Z".into(),
        pref: LogonPref::Original,
    });
    assert_eq!(start(&mut pc, &mut g, 0), None);
    assert_eq!(g.state.pref_held, None);
    assert_eq!(g.alarms.all().len(), 1);
    // A run that came while the guard runs is taken by the hourly look.
    pc.logon = Some(Logon {
        at: "2026-10-01T06:00:00.0000000Z".into(),
        pref: LogonPref::Held(PrefHeld {
            value: Some("32".into()),
            by: CardHolders {
                reaper: false,
                names: "spike.exe (99)".into(),
            },
        }),
    });
    tick(&mut pc, &mut g, Instant::now());
    assert_eq!(g.alarms.all().len(), 2);
    assert_eq!(
        g.alarms.last().unwrap().text,
        "logon task: the driver module is held by spike.exe (99) with the preferred buffer at \
         32; nothing was written"
    );
}

/// After a reboot with REAPER holding the card at 32, the logon task's run
/// and the event plan's check find the same: one alarm, nothing written,
/// REAPER left alone.
#[test]
fn the_logon_task_and_the_event_plan_alarm_once_for_the_same_value() {
    let mut g = Guard::for_test(Mode::Dev);
    g.state.written_at = 1_000;
    let mut pc = FakePc::new(band_up());
    pc.pref_attempts = 1;
    pc.logon = Some(logon_held("2026-09-28T06:00:00.0000000Z"));
    assert_eq!(start(&mut pc, &mut g, 5_000), Some(Outcome::Done));
    assert_eq!(g.state.mode, Mode::Event);
    assert!(pc.called(Call::PrefCheck));
    assert_eq!(pc.pref_writes, 0);
    assert!(!pc.called(Call::ReaperStart) && !pc.called(Call::ReaperSaveQuit));
    assert_eq!(texts(&g), [format!("logon task: {HELD}")]);
    assert_eq!(g.state.pref_held.as_deref(), Some(HELD));
}

#[test]
fn a_failed_dev_step_alarms_and_unwinds_to_event() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    pc.fail(Call::Data, "iem-migrate band ended with Some(1)");
    let r = handle(&mut pc, &mut g, dev(), INIT);
    assert!(!r.ok);
    assert!(
        r.detail.starts_with("dev: not entered; unwound to event"),
        "{}",
        r.detail
    );
    assert!(
        r.detail
            .contains("unwinding to event: iem-migrate band ended with Some(1)")
    );
    assert_eq!(g.state.mode, Mode::Event);
    assert!(!pc.called(Call::EngineStart));
    assert!(pc.index(Call::ReaperStart) > pc.index(Call::Data));
    assert!(pc.index(Call::AppStart) > pc.index(Call::ReaperStart));
    let a = &g.alarms.all()[0];
    assert_eq!(a.step, Some(Step::Data));
    assert_eq!(a.text, "Data: iem-migrate band ended with Some(1)");
    assert!(!a.owner_question);
    assert_eq!(g.shared.view().epoch, 2);
    // The entry and its unwind (event → event, no start's checks) each moved
    // the fence (#42).
    let v = g.shared.view();
    assert_eq!(
        (v.fence, v.start_checks, v.began),
        (2, false, Some((Mode::Event, Mode::Event)))
    );
}

#[test]
fn a_failed_arm_readiness_or_identity_unwinds() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    // No active bundle: the identity check cannot name one.
    let r = handle(&mut pc, &mut g, dev(), INIT);
    assert!(!r.ok);
    assert_eq!(g.alarms.all()[0].text, "IdentityCheck: no active bundle");
    assert!(pc.called(Call::EngineStop));
    assert!(!pc.called(Call::Identity));
    assert_eq!(g.state.mode, Mode::Event);
}

#[test]
fn engine_ready_restarts_its_window_once() {
    // The window every `engine_ready` runs (effects::engine::ReadyWindow):
    // one warm-up miss restarts the 10 s window once, a second miss fails.
    let status = |callbacks: u64, missed: u64| Status {
        frames: 32,
        callbacks,
        missed,
        ..Status::default()
    };
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let mut w = ReadyWindow::new(READY_S);
    assert_eq!(w.observe(&status(100, 0), at(0)), Ready::Wait);
    assert_eq!(w.observe(&status(200, 1), at(1)), Ready::Wait);
    assert_eq!(w.observe(&status(300, 1), at(10)), Ready::Wait);
    assert_eq!(w.observe(&status(400, 1), at(11)), Ready::Done);
    let mut w = ReadyWindow::new(READY_S);
    w.observe(&status(100, 0), at(0));
    w.observe(&status(200, 1), at(1));
    assert!(matches!(
        w.observe(&status(300, 2), at(2)),
        Ready::Failed(_)
    ));
    // The arm step: the 10 s readiness, then Arm; a failed readiness never
    // arms and unwinds to event.
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    pc.fail(
        Call::EngineReady,
        "2 periods missed after the warm-up restart",
    );
    let r = handle(&mut pc, &mut g, dev(), INIT);
    assert!(!r.ok);
    assert_eq!(pc.ready_secs, [10]);
    assert!(!pc.called(Call::EngineArm));
    assert!(pc.index(Call::EngineStop) > pc.index(Call::EngineReady));
    assert_eq!(
        g.alarms.all()[0].text,
        "EngineArm: 2 periods missed after the warm-up restart"
    );
}

#[test]
fn an_engine_that_finds_its_state_directory_busy_is_started_again_within_the_step() {
    // F3 round 4, finding 4: exit 75 was handled by the crash watch only.
    // An engine EngineStart started that ended with 75 (its state
    // directory still held a moment) left EngineArm waiting for its pipe,
    // and the whole entry unwound. The ready wait notices the exit, and the
    // step starts the engine again, held, once, after BUSY_RETRY.
    let entry = |exits: Vec<Option<i32>>| {
        let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
        g.state.pins.current = Some(SHA.into());
        pc.early_exits = exits;
        let t0 = Instant::now();
        let r = handle(&mut pc, &mut g, dev(), INIT);
        (pc, g, r, t0.elapsed())
    };
    let (pc, g, r, took) = entry(vec![Some(75)]);
    assert!(r.ok, "{r:?}");
    assert!(took >= crash::BUSY_RETRY, "{took:?}");
    assert_eq!(pc.engine_starts, [(true, false), (true, false)]);
    assert_eq!(pc.count(Call::EngineReady), 2);
    assert_eq!(pc.count(Call::PrefCheck), 2);
    assert_eq!(g.spawns, 2);
    assert_eq!(g.state.mode, Mode::Dev);
    assert!(g.alarms.all().is_empty(), "{:?}", texts(&g));
    // A second busy exit, or any other end, fails the step: it unwinds.
    for (exits, starts) in [(vec![Some(75), Some(75)], 2), (vec![Some(70)], 1)] {
        let (pc, g, r, _) = entry(exits.clone());
        assert!(!r.ok, "{exits:?}: {r:?}");
        assert_eq!(pc.engine_starts.len(), starts, "{exits:?}");
        assert_eq!(g.spawns, starts as u64, "{exits:?}");
        assert!(!pc.called(Call::EngineArm), "{exits:?}");
        assert_eq!(g.state.mode, Mode::Event, "{exits:?}");
    }
}

#[test]
fn a_preempted_token_sends_a_dev_switch_back_before_its_first_step() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.cancel.preempt();
    let out = run_switch(&mut pc, &mut g, Mode::Event, Mode::Dev);
    assert_eq!(out, Outcome::Done);
    assert!(!pc.called(Call::Precheck));
    assert_eq!(g.state.mode, Mode::Event);
    // The event plan cleared the token as it began.
    assert!(!g.cancel.preempted());
    assert!(pc.called(Call::Fingerprint));
}

#[test]
fn ide_event_during_the_last_step_of_a_dev_switch_goes_to_event() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    // The last step is a mutation: it ignores the token and finishes.
    pc.delay(Call::RunnerStart, Duration::from_millis(300));
    let fired = preempt_after(&g, Step::IdentityCheck);
    let out = run_switch(&mut pc, &mut g, Mode::Event, Mode::Dev);
    fired.join().unwrap();
    assert_eq!(out, Outcome::Done);
    assert!(pc.index(Call::ReaperStart) > pc.index(Call::RunnerStart));
    assert!(pc.index(Call::AppStart) > pc.index(Call::ReaperStart));
    assert_eq!(g.state.mode, Mode::Event);
    assert_eq!(g.state.switching, None);
    assert!(!g.cancel.preempted());
    let v = g.shared.view();
    assert_eq!(
        (v.mode, v.running, v.last),
        (Mode::Event, None, Some(Outcome::Done))
    );
    // The rehearsal's re-entry into dev failing after "ide event" came:
    // REAPER starts (the owner said event), no owner question.
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    g.state.pins.current = Some(SHA.into());
    pc.delay(Call::ServerStart, Duration::from_millis(300));
    pc.fail(Call::ServerStart, "ports 80/443 are still held");
    let fired = preempt_after(&g, Step::EngineArm);
    let r = handle(&mut pc, &mut g, Request::RehearseTeardown, INIT);
    fired.join().unwrap();
    assert!(!r.ok);
    assert!(
        r.detail.contains("dev: not entered; unwound to event"),
        "{}",
        r.detail
    );
    assert_eq!(g.state.mode, Mode::Event);
    assert!(pc.called(Call::ReaperStart) && pc.called(Call::AppStart));
    assert!(!g.cancel.preempted());
    assert!(
        g.alarms.iter().all(|a| !a.owner_question),
        "{:?}",
        texts(&g)
    );
}

#[test]
fn an_event_plan_clears_the_token_as_it_begins() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.cancel.preempt();
    pc.block_until_cancel(Call::ReaperFacts);
    let c = g.cancel.clone();
    let t = Instant::now();
    let fired = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        c.preempt();
    });
    // A pre-emption during an event plan ends only that wait: the plan goes
    // on, and a handover cut short is no done (#10: the owner decides).
    let out = run_switch(&mut pc, &mut g, Mode::Event, Mode::Event);
    fired.join().unwrap();
    assert!(t.elapsed() >= Duration::from_millis(200));
    assert_eq!(out, Outcome::NeedsOwner);
    assert_eq!(
        g.alarms.all()[0].text,
        "ReaperHandover: pre-empted inside the event plan"
    );
    assert!(g.alarms.all()[0].owner_question);
    assert!(pc.called(Call::AppAnswers) && pc.called(Call::Fingerprint));
}

#[test]
fn event_plan_failures_follow_the_policy() {
    // A failed app stop skips the app start; the handover still runs. No
    // app serves then, so the owner is asked (#10).
    let (mut pc, mut g) = (
        FakePc::new(Facts {
            app_serves: false,
            ..band_up()
        }),
        Guard::for_test(Mode::Event),
    );
    pc.app_exit.exit_code = None;
    assert_eq!(
        run_switch(&mut pc, &mut g, Mode::Event, Mode::Event),
        Outcome::NeedsOwner
    );
    assert!(pc.called(Call::AppStop) && !pc.called(Call::AppStart));
    assert!(pc.called(Call::AppAnswers) && pc.called(Call::Fingerprint));
    assert_eq!(texts(&g), ["AppStop: the app did not exit within 30 s"]);
    assert!(g.alarms.last().unwrap().owner_question);
    // A REAPER handover that fails asks the owner and goes on to the app;
    // the switch never ends done (#10).
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Dev));
    pc.reaper.dialogs = vec!["Save changes?".into()];
    pc.reaper.heartbeat_advanced = false;
    assert_eq!(
        run_switch(&mut pc, &mut g, Mode::Dev, Mode::Event),
        Outcome::NeedsOwner
    );
    assert_eq!(g.state.mode, Mode::Event);
    assert!(pc.called(Call::AppStart));
    assert_eq!(
        texts(&g),
        ["ReaperHandover: a REAPER dialog is open; the meter heartbeat does not advance"]
    );
    assert!(g.alarms.last().unwrap().owner_question);
    // A foreign holder that stays keeps REAPER down and asks the owner.
    let (mut pc, mut g) = (
        FakePc::new(Facts {
            other_module_holder: true,
            ..Facts::default()
        }),
        Guard::for_test(Mode::Dev),
    );
    pc.fail(Call::HolderGone, "still held after 30 s");
    assert_eq!(
        run_switch(&mut pc, &mut g, Mode::Dev, Mode::Event),
        Outcome::NeedsOwner
    );
    assert!(!pc.called(Call::ReaperStart) && !pc.called(Call::AppStart));
    assert!(g.alarms.last().unwrap().owner_question);
    // REAPER that does not start: no app without REAPER.
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Dev));
    pc.fail(Call::ReaperStart, "the task did not run");
    assert_eq!(
        run_switch(&mut pc, &mut g, Mode::Dev, Mode::Event),
        Outcome::NeedsOwner
    );
    assert!(!pc.called(Call::AppStart));
    // A failed tuning exit alarms and goes on.
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Dev));
    pc.fail(Call::Tuning, "no answer within 120 s");
    assert_eq!(
        run_switch(&mut pc, &mut g, Mode::Dev, Mode::Event),
        Outcome::Done
    );
    assert!(pc.called(Call::ReaperStart));
    assert_eq!(texts(&g), ["TuningExit: no answer within 120 s"]);
    // Only a failed release reads the engine's health.
    assert!(!pc.called(Call::EngineHealth));
}

#[test]
fn the_handover_reports_what_it_saw() {
    // REAPER and the app down: nothing holds the driver, so the preference
    // is restored before REAPER starts (never under a REAPER that holds it,
    // #9 2026-09-28).
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    pc.reaper.peaks = vec![f64::NEG_INFINITY];
    pc.pref_attempts = 2;
    let r = handle(
        &mut pc,
        &mut g,
        Request::Event {
            dry_run: false,
            signal: false,
        },
        INIT,
    );
    assert!(r.ok, "{r:?}");
    assert_eq!(
        r.detail,
        "event: done; tuning exit: exit: ok; the preferred buffer was restored (2 writes); \
         UNCONFIRMED-AUDIO: every stage input at the meter floor"
    );
    // A preference already at the original is not reported.
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    let r = handle(
        &mut pc,
        &mut g,
        Request::Event {
            dry_run: false,
            signal: false,
        },
        INIT,
    );
    assert_eq!(r.detail, "event: done; tuning exit: exit: ok");
}

/// REAPER's evaluation notice as the first guard start on the PC saw it
/// (#9, 2026-09-28): REAPER shows it at every start and runs normally with
/// it open.
const NOTICE: &str = "About REAPER v7.65/win64 rev 0a1b2c";

#[test]
fn reapers_evaluation_notice_is_reported_and_never_an_alarm() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    pc.reaper.dialogs = vec![NOTICE.into()];
    let r = ask(
        &mut pc,
        &mut g,
        Request::Event {
            dry_run: false,
            signal: false,
        },
    );
    assert!(r.ok, "{r:?}");
    assert_eq!(
        r.detail,
        r#"event: done; tuning exit: exit: ok; reaper_notice: "evaluation""#
    );
    assert!(g.alarms.all().is_empty(), "{:?}", texts(&g));
    assert_eq!(
        status_reply(&g),
        r#"mode event; no bundle; reaper_notice: "evaluation""#
    );
    assert_eq!(
        ask(&mut pc, &mut g, Request::Status).detail,
        r#"mode event; no bundle; reaper_notice: "evaluation""#
    );
    // Any other dialog beside it fails the handover as before; the report
    // still names the notice.
    pc.reaper.dialogs.push("Save changes?".into());
    let r = ask(
        &mut pc,
        &mut g,
        Request::Event {
            dry_run: false,
            signal: false,
        },
    );
    assert_eq!(texts(&g), ["ReaperHandover: a REAPER dialog is open"]);
    assert!(
        r.detail.contains(r#"; reaper_notice: "evaluation""#),
        "{}",
        r.detail
    );
    // Without it the report and the status do not name it.
    pc.reaper.dialogs.clear();
    let r = ask(
        &mut pc,
        &mut g,
        Request::Event {
            dry_run: false,
            signal: false,
        },
    );
    assert!(!r.detail.contains("reaper_notice"), "{}", r.detail);
    assert!(
        !status_reply(&g).contains("reaper_notice"),
        "{}",
        status_reply(&g)
    );
}

#[test]
fn the_status_names_the_notice_only_as_the_last_handover_saw_it() {
    // A dev entry saves and quits REAPER: the notice goes with it.
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    pc.reaper.dialogs = vec![NOTICE.into()];
    assert!(
        ask(
            &mut pc,
            &mut g,
            Request::Event {
                dry_run: false,
                signal: false,
            }
        )
        .ok
    );
    assert!(
        status_reply(&g).contains("reaper_notice"),
        "{}",
        status_reply(&g)
    );
    let r = ask(&mut pc, &mut g, dev());
    assert!(r.ok, "{r:?}");
    assert!(
        !status_reply(&g).contains("reaper_notice"),
        "{}",
        status_reply(&g)
    );
    assert!(
        !status_text(&g).contains("reaper_notice"),
        "{}",
        status_text(&g)
    );
    // A handover that cannot read REAPER does not repeat an old notice.
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    pc.reaper.dialogs = vec![NOTICE.into()];
    assert!(
        ask(
            &mut pc,
            &mut g,
            Request::Event {
                dry_run: false,
                signal: false,
            }
        )
        .ok
    );
    pc.fail(Call::ReaperFacts, "REAPER's windows: access denied");
    let r = ask(
        &mut pc,
        &mut g,
        Request::Event {
            dry_run: false,
            signal: false,
        },
    );
    assert!(!r.detail.contains("reaper_notice"), "{}", r.detail);
    assert_eq!(
        texts(&g),
        ["ReaperHandover: REAPER's windows: access denied"]
    );
    assert!(
        !status_reply(&g).contains("reaper_notice"),
        "{}",
        status_reply(&g)
    );
}

#[test]
fn the_jobs_are_cancelled_before_the_runner_stops() {
    let (mut pc, mut g) = (
        FakePc::new(Facts {
            runner: true,
            ..iemmixer_up()
        }),
        Guard::for_test(Mode::Dev),
    );
    g.state.job = Some(4242);
    let r = handle(
        &mut pc,
        &mut g,
        Request::Event {
            dry_run: false,
            signal: false,
        },
        INIT,
    );
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.job, None);
    assert!(r.detail.contains("HIL job 4242 cancelled"), "{}", r.detail);
    assert!(pc.index(Call::RunnerStop) < pc.index(Call::EngineStop));
}
