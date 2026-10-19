//! The switch runner against `FakePc` (`daemon/runner.rs`): a dev entry's
//! plan, the preference check before REAPER and before the engine, the
//! logon task's result, the unwind of a failed entry, the engine's
//! readiness (the boot's live entry's attempts in prod, #11), "ide event"'s
//! pre-emption and the event plan's error policy.

use std::time::{Duration, Instant};

use super::tests::{
    HELD, INIT, SHA, ask, band_up, dev, event, iemmixer_up, no_reaper, preempt_after, prod_on,
    record, status_reply, steps, texts,
};
use super::*;
use crate::bundle::Hil;
use crate::crash;
use crate::effects::engine::{Ready, ReadyWindow};
use crate::effects::tuning::{Logon, LogonPref};
use crate::lifecycle::Lifecycle;
use crate::pc::fake::{Call, FakePc};
use crate::pc::{CardHolders, Kid, PrefHeld, Status};
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
    let r = handle(&mut pc, &mut g, event(false), INIT);
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
        let r = ask(&mut pc, &mut g, event(false));
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
    assert!(ask(&mut pc, &mut g, event(false)).ok);
    let r = ask(&mut pc, &mut g, event(false));
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
    let r = ask(&mut pc, &mut g, event(false));
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
    assert!(ask(&mut pc, &mut g, event(false)).ok);
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
    let r = ask(&mut pc, &mut g, event(false));
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
    assert_eq!(w.observe(&status(300, 2), at(2)), Ready::Missed(1));
    // The arm step: the 10 s readiness, then Arm; a failed readiness never
    // arms and unwinds to event. A dev entry gets one attempt (only the
    // boot's live entry in prod starts the engine again, #11 2026-10-19).
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    pc.ready_misses = vec![2];
    let r = handle(&mut pc, &mut g, dev(), INIT);
    assert!(!r.ok);
    assert_eq!(pc.ready_secs, [10]);
    assert_eq!(pc.engine_starts.len(), 1);
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

// ---- the boot's live entry in prod: an engine that missed periods (#11) ----

/// The boot after which the guards below start: later than their state.
const BOOT: u64 = 5_000;

/// A guard live in prod on the pin `SHA` (a green `main` build) whose state
/// was written before [`BOOT`], on a PC where nothing runs: its start is the
/// boot's live entry on the pin (`startup::start_plan`). Its attempts wait
/// 10 ms between them, not `BOOT_ARM_SPACING`.
fn booted_in_prod() -> (FakePc, Guard) {
    let mut g = Guard::for_test(Mode::Live);
    g.state.written_at = 1_000;
    g.state.lifecycle = prod_on(None, None);
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Green));
    g.boot_arm_spacing = Duration::from_millis(10);
    (FakePc::new(Facts::default()), g)
}

/// `iemmode live` in prod: the pin's live entry.
fn live() -> Request {
    Request::Live {
        build: None,
        trial: false,
        dry_run: false,
    }
}

/// The texts of the `EngineArm` alarms, oldest first.
fn arm_alarms(g: &Guard) -> Vec<String> {
    g.alarms
        .iter()
        .filter(|a| a.step == Some(Step::EngineArm))
        .map(|a| a.text.clone())
        .collect()
}

/// #11, 2026-10-19, right after the cutover: after a graceful reboot in
/// prod the boot's live entry started the engine 30 s after Windows booted,
/// and 7 s later its arm failed with "1 periods missed after the warm-up
/// restart" (the boot's own load: every manual switch armed with none), so
/// the PC unwound to REAPER. In the boot's entry such an engine is stopped,
/// the boot's load gets `boot_arm_spacing`, the preference is checked and
/// the engine starts again, held, inside the step; the window is the same
/// at every attempt, and each attempt is logged.
#[test]
fn the_boot_s_live_entry_starts_an_engine_that_missed_periods_again() {
    let (mut pc, mut g) = booted_in_prod();
    pc.ready_misses = vec![1, 2];
    assert_eq!(start(&mut pc, &mut g, BOOT), Some(Outcome::Done));
    assert_eq!(g.state.mode, Mode::Live);
    assert_eq!(pc.engine_starts, [(true, false); 3]);
    assert_eq!(g.spawns, 3);
    assert_eq!(pc.ready_secs, [READY_S; 3], "the same window every time");
    assert!(pc.ready_misses.is_empty());
    assert_eq!(pc.count(Call::EngineArm), 1);
    let arm: Vec<Call> = steps(&pc)
        .into_iter()
        .skip_while(|c| *c != Call::EngineStart)
        .take_while(|c| *c != Call::ServerStart)
        .collect();
    assert_eq!(
        arm,
        [
            Call::EngineStart,
            Call::EngineReady,
            Call::EngineStop,
            Call::PrefCheck,
            Call::EngineStart,
            Call::EngineReady,
            Call::EngineStop,
            Call::PrefCheck,
            Call::EngineStart,
            Call::EngineReady,
            Call::EngineArm,
        ]
    );
    // No event plan ran, and nothing alarmed.
    assert!(!pc.called(Call::ReaperStart) && !pc.called(Call::AppStart));
    assert!(g.alarms.all().is_empty(), "{:?}", texts(&g));
    let said: Vec<&str> = g
        .report
        .iter()
        .map(String::as_str)
        .filter(|l| l.contains(" of 4"))
        .collect();
    assert_eq!(
        said,
        [
            "the boot's live entry, attempt 1 of 4: 1 periods missed after the warm-up restart; \
             the engine stops and starts again after 10ms",
            "engine started again, held (pid 1002): attempt 2 of 4",
            "the boot's live entry, attempt 2 of 4: 2 periods missed after the warm-up restart; \
             the engine stops and starts again after 10ms",
            "engine started again, held (pid 1003): attempt 3 of 4",
        ]
    );
}

/// The boot's attempts are bounded: after the plan's start and
/// `BOOT_ARM_RETRIES` more, a miss fails the step and the alarm names the
/// attempts. Since the owner's decision of 2026-10-19 (#11: in prod nothing
/// goes to REAPER on its own) the entry stops there: the PC stays in event,
/// REAPER and the app are not started, the alarm names the ways on. The
/// boot's entry ends with its switch: a later manual live entry gets one
/// attempt.
#[test]
fn the_boot_s_live_entry_stops_after_its_last_attempt() {
    assert_eq!(BOOT_ARM_RETRIES, 3);
    let (mut pc, mut g) = booted_in_prod();
    pc.ready_misses = vec![1; 4];
    assert_eq!(start(&mut pc, &mut g, BOOT), Some(Outcome::NeedsOwner));
    assert_eq!(g.state.mode, Mode::Event);
    assert_eq!(g.state.switching, None);
    assert_eq!(pc.engine_starts.len(), 4);
    assert_eq!(g.spawns, 4);
    assert_eq!(pc.count(Call::EngineReady), 4);
    assert!(pc.ready_misses.is_empty());
    assert!(!pc.called(Call::EngineArm) && !pc.called(Call::ServerStart));
    assert!(!pc.called(Call::ReaperStart) && !pc.called(Call::AppStart));
    assert_eq!(
        arm_alarms(&g),
        [format!(
            "EngineArm: 1 periods missed after the warm-up restart (4 attempts after the boot); \
             {}",
            no_reaper(SHA)
        )]
    );
    assert!(g.alarms.last().unwrap().owner_question);
    assert_eq!(g.state.lifecycle, prod_on(None, None));
    pc.ready_misses = vec![1, 1];
    let r = ask(&mut pc, &mut g, live());
    assert!(!r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Event);
    assert_eq!(pc.engine_starts.len(), 5);
    assert_eq!(pc.ready_misses, [1]);
    assert!(!pc.called(Call::ReaperStart));
    let manual = format!(
        "EngineArm: 1 periods missed after the warm-up restart; {}",
        no_reaper(SHA)
    );
    assert_eq!(arm_alarms(&g).last(), Some(&manual));
}

/// Only the boot's entry gets more attempts: a manual live entry in prod
/// and a live trial fail at their first miss after the warm-up restart, as
/// before (a dev entry: `engine_ready_restarts_its_window_once`). The trial
/// unwinds to event; the prod entry stops in the mode it began from (the
/// owner's decision on #11, 2026-10-19: in prod nothing goes to REAPER on
/// its own).
#[test]
fn a_manual_live_entry_or_a_trial_fails_at_its_first_miss() {
    let trial = Request::Live {
        build: Some(SHA.into()),
        trial: true,
        dry_run: false,
    };
    let cases = [
        (
            prod_on(None, None),
            Mode::Dev,
            iemmixer_up(),
            live(),
            format!("; {}", no_reaper(SHA)),
        ),
        (
            Lifecycle::Trial,
            Mode::Event,
            band_up(),
            trial,
            String::new(),
        ),
    ];
    for (lifecycle, mode, facts, entry, ways) in cases {
        let (mut pc, mut g) = (FakePc::new(facts), Guard::for_test(mode));
        g.state.lifecycle = lifecycle.clone();
        g.state
            .bundles
            .insert(SHA.into(), record(SHA, "main", Hil::Green));
        g.boot_arm_spacing = Duration::from_millis(10);
        pc.ready_misses = vec![1; 4];
        let r = handle(&mut pc, &mut g, entry, INIT);
        assert!(!r.ok, "{lifecycle:?}: {r:?}");
        assert_eq!(g.state.mode, mode, "{lifecycle:?}");
        assert_eq!(pc.engine_starts.len(), 1, "{lifecycle:?}");
        assert_eq!(pc.count(Call::EngineReady), 1, "{lifecycle:?}");
        assert_eq!(pc.ready_misses, [1; 3], "{lifecycle:?}");
        assert!(!pc.called(Call::EngineArm), "{lifecycle:?}");
        assert_eq!(
            arm_alarms(&g),
            [format!(
                "EngineArm: 1 periods missed after the warm-up restart{ways}"
            )],
            "{lifecycle:?}"
        );
        assert!(!r.detail.contains("attempt"), "{lifecycle:?}: {}", r.detail);
    }
}

/// Only missed periods are the boot's load: any other failure of the boot's
/// ready wait (a fault, a parked stream, another period, callbacks that
/// stopped, an engine that ended) fails the step at once, and the entry
/// stops in event without REAPER (the owner's decision on #11, 2026-10-19).
#[test]
fn the_boot_s_live_entry_retries_only_missed_periods() {
    let (mut pc, mut g) = booted_in_prod();
    pc.fail(Call::EngineReady, "the engine faulted");
    assert_eq!(start(&mut pc, &mut g, BOOT), Some(Outcome::NeedsOwner));
    assert_eq!(g.state.mode, Mode::Event);
    assert_eq!(pc.engine_starts.len(), 1);
    assert_eq!(pc.count(Call::EngineReady), 1);
    assert!(!pc.called(Call::EngineArm) && !pc.called(Call::ReaperStart));
    assert_eq!(
        arm_alarms(&g),
        [format!("EngineArm: the engine faulted; {}", no_reaper(SHA))]
    );
    let (mut pc, mut g) = booted_in_prod();
    pc.early_exits = vec![Some(70)];
    assert_eq!(start(&mut pc, &mut g, BOOT), Some(Outcome::NeedsOwner));
    assert_eq!(g.state.mode, Mode::Event);
    assert_eq!(pc.engine_starts.len(), 1);
    assert!(!pc.called(Call::ReaperStart));
    assert_eq!(
        arm_alarms(&g),
        [format!(
            "EngineArm: the engine ended (Some(70)) before it was ready; {}",
            no_reaper(SHA)
        )]
    );
}

/// What the stop of a failed prod entry leaves (the owner's decision on
/// #11, 2026-10-19: in prod nothing goes to REAPER on its own): the boot's
/// entry that failed at its arm stops in event, its held engine running,
/// no switch left. The crash watch starts nothing there (no respawn in
/// event), and both ways on run from it: a live entry on the pin, and the
/// explicit event plan, the PC staying prod.
#[test]
fn a_stopped_boot_entry_leaves_a_state_both_ways_on_run_from() {
    let stopped = || {
        let (mut pc, mut g) = booted_in_prod();
        pc.fail(Call::EngineReady, "the engine faulted");
        assert_eq!(start(&mut pc, &mut g, BOOT), Some(Outcome::NeedsOwner));
        pc.heal(Call::EngineReady);
        (pc, g)
    };
    let (mut pc, mut g) = stopped();
    assert_eq!(
        (g.state.mode, g.state.switching.clone()),
        (Mode::Event, None)
    );
    assert!(pc.facts.engine, "the held engine runs");
    let v = g.shared.view();
    assert_eq!((v.running, v.last), (None, Some(Outcome::NeedsOwner)));
    // Its engine ends: nothing starts again, no REAPER.
    pc.exited = vec![(Kid::Engine, Some(70))];
    pc.facts.engine = false;
    let at = Instant::now();
    tick(&mut pc, &mut g, at);
    tick(&mut pc, &mut g, at + Duration::from_secs(20));
    assert_eq!(pc.engine_starts.len(), 1);
    assert!(!pc.called(Call::ReaperStart) && !pc.called(Call::AppStart));
    assert_eq!(g.state.mode, Mode::Event);
    // `iemmode live --build <pin>`: live on the pin.
    let (mut pc, mut g) = stopped();
    let live_on_pin = Request::Live {
        build: Some(SHA.into()),
        trial: false,
        dry_run: false,
    };
    let r = ask(&mut pc, &mut g, live_on_pin);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Live);
    // The held engine is stopped before the pin's engine starts.
    assert_eq!(pc.engine_starts.len(), 2);
    assert!(pc.called(Call::EngineStop) && pc.called(Call::EngineArm));
    assert!(!pc.called(Call::ReaperStart) && !pc.called(Call::AppStart));
    // `iemmode event`: REAPER by hand, the PC stays prod.
    let (mut pc, mut g) = stopped();
    let r = ask(&mut pc, &mut g, event(false));
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Event);
    assert!(pc.called(Call::EngineStop));
    assert!(pc.called(Call::ReaperStart) && pc.called(Call::AppStart));
    assert_eq!(g.state.lifecycle, prod_on(None, None));
}

/// A pre-empted entry keeps the event plan in prod too: "ide event" (the
/// button) that came during a step which then failed is the request for it
/// (#11, 2026-10-19: only what runs on its own stops).
#[test]
fn in_prod_an_entry_preempted_during_a_failing_step_still_unwinds() {
    let (mut pc, mut g) = booted_in_prod();
    pc.preempt_at = Some((Call::ServerStart, g.cancel.clone()));
    pc.fail(Call::ServerStart, "ports 80/443 are still held");
    assert_eq!(start(&mut pc, &mut g, BOOT), Some(Outcome::Done));
    assert_eq!(g.state.mode, Mode::Event);
    assert!(pc.called(Call::ReaperStart) && pc.called(Call::AppStart));
    assert_eq!(texts(&g), ["ServerStart: ports 80/443 are still held"]);
    assert!(!g.alarms.last().unwrap().owner_question);
}

/// The wait between two of the boot's attempts takes the switch's token:
/// a pre-emption ends it at once and the entry unwinds, no further start.
#[test]
fn a_preemption_ends_the_boot_s_wait_between_two_attempts() {
    let (mut pc, mut g) = booted_in_prod();
    g.boot_arm_spacing = Duration::from_secs(30);
    pc.ready_misses = vec![1];
    pc.preempt_at = Some((Call::EngineStop, g.cancel.clone()));
    let t = Instant::now();
    assert_eq!(start(&mut pc, &mut g, BOOT), Some(Outcome::Done));
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    assert_eq!(g.state.mode, Mode::Event);
    assert_eq!(pc.engine_starts.len(), 1);
    assert!(!pc.called(Call::EngineArm));
    assert!(pc.called(Call::ReaperStart) && pc.called(Call::AppStart));
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
    let r = handle(&mut pc, &mut g, event(false), INIT);
    assert!(r.ok, "{r:?}");
    assert_eq!(
        r.detail,
        "event: done; tuning exit: exit: ok; the preferred buffer was restored (2 writes); \
         UNCONFIRMED-AUDIO: every stage input at the meter floor"
    );
    // A preference already at the original is not reported.
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    let r = handle(&mut pc, &mut g, event(false), INIT);
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
    let r = ask(&mut pc, &mut g, event(false));
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
    let r = ask(&mut pc, &mut g, event(false));
    assert_eq!(texts(&g), ["ReaperHandover: a REAPER dialog is open"]);
    assert!(
        r.detail.contains(r#"; reaper_notice: "evaluation""#),
        "{}",
        r.detail
    );
    // Without it the report and the status do not name it.
    pc.reaper.dialogs.clear();
    let r = ask(&mut pc, &mut g, event(false));
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
    assert!(ask(&mut pc, &mut g, event(false)).ok);
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
    assert!(ask(&mut pc, &mut g, event(false)).ok);
    pc.fail(Call::ReaperFacts, "REAPER's windows: access denied");
    let r = ask(&mut pc, &mut g, event(false));
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
    let r = handle(&mut pc, &mut g, event(false), INIT);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.job, None);
    assert!(r.detail.contains("HIL job 4242 cancelled"), "{}", r.detail);
    assert!(pc.index(Call::RunnerStop) < pc.index(Call::EngineStop));
}
