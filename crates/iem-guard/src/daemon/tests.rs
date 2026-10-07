//! The daemon against `FakePc` (S6 plan Task 10 Step 5): the switch runner's
//! pre-emption and error policy, the requests, the watch, the start after a
//! reboot or a restart, and the `--direct` path.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use iem_win::spawn::Placement;

use super::*;
use crate::bundle::Pins;
use crate::effects::engine::{Ready, ReadyWindow};
use crate::effects::tuning::{Logon, LogonPref};
use crate::install;
use crate::pc::fake::{Call, FakePc};
use crate::pc::{CardHolders, PrefHeld, Status};
use crate::plan::Facts;
use crate::proto::GUARD_BUILD;

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const OTHER: &str = "89abcdef0123456789abcdef0123456789abcdef";
const T0: u64 = 1_790_000_000;

fn band_up() -> Facts {
    Facts {
        reaper: true,
        app: true,
        reaper_holds_module: true,
        app_serves: true,
        ..Facts::default()
    }
}

fn iemmixer_up() -> Facts {
    Facts {
        engine: true,
        server: true,
        tray: true,
        ..Facts::default()
    }
}

fn dev() -> Request {
    Request::Dev {
        build: None,
        dry_run: false,
    }
}

fn record(sha: &str, branch: &str, hil: Hil) -> Record {
    Record {
        sha: sha.to_owned(),
        branch: branch.to_owned(),
        run: 4242,
        installed_at: T0,
        hil,
    }
}

/// The calls without the reads every step makes (facts, children).
fn steps(pc: &FakePc) -> Vec<Call> {
    pc.calls()
        .into_iter()
        .filter(|c| !matches!(c, Call::Facts | Call::Children))
        .collect()
}

fn texts(g: &Guard) -> Vec<String> {
    g.alarms.iter().map(|a| a.text.clone()).collect()
}

/// A request sent after the one before it was answered: the pipe queues it
/// with the switch generation of that moment (`Shared::route`). A literal
/// generation after a switch began (a request, the watch's crash fallback) is a
/// request queued before that switch, answered as during it (`stale`).
fn ask(pc: &mut FakePc, g: &mut Guard, req: Request) -> Reply {
    let epoch = g.shared.epoch();
    handle(pc, g, req, epoch)
}

// ---- the plan's exact tests ----

#[test]
fn preempt_during_a_waiting_step_starts_event_within_1s() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    pc.block_until_cancel(Call::AppStop);
    let c = g.cancel.clone();
    let fired = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        c.preempt();
        Instant::now()
    });
    run_switch(&mut pc, &mut g, Mode::Event, Mode::Dev);
    let at = fired.join().unwrap();
    assert!(!pc.called(Call::ReaperSaveQuit));
    let first_event_call = pc.first_after(at).expect("the event plan ran");
    assert!(first_event_call.1.duration_since(at) < Duration::from_secs(1));
    assert_eq!(g.state.mode, Mode::Event);
}

#[test]
fn event_preempts_a_running_dev_switch() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    pc.delay(Call::EngineStart, Duration::from_millis(300)); // a mutating step finishes
    let c = g.cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        c.preempt();
    });
    run_switch(&mut pc, &mut g, Mode::Event, Mode::Dev);
    assert!(pc.called(Call::EngineStart));
    assert!(!pc.called(Call::EngineArm));
    assert!(pc.index(Call::EngineStop) > pc.index(Call::EngineStart));
    assert!(pc.called(Call::ReaperStart) && pc.called(Call::AppStart));
}

#[test]
fn a_healthy_engine_keeps_serving_when_release_times_out() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    pc.fail(Call::EngineStop, "no DriverReleased within 10 s");
    pc.health(Health::Healthy);
    assert_eq!(
        run_switch(&mut pc, &mut g, Mode::Dev, Mode::Event),
        Outcome::KeptServing
    );
    for c in [
        Call::ServerStop,
        Call::TrayStop,
        Call::ReaperStart,
        Call::AppStart,
    ] {
        assert!(!pc.called(c), "{c:?} after a failed release");
    }
    assert_eq!(g.state.mode, Mode::Dev);
    assert!(g.alarms.last().unwrap().owner_question);
}

#[test]
fn a_parked_engine_stops_the_plan_and_asks_the_owner() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    pc.fail(Call::EngineStop, "no DriverReleased within 10 s");
    pc.health(Health::Parked);
    assert_eq!(
        run_switch(&mut pc, &mut g, Mode::Dev, Mode::Event),
        Outcome::NeedsOwner
    );
    assert_eq!(pc.calls_after(Call::EngineHealth), Vec::<Call>::new());
}

#[test]
fn a_preemption_is_no_failure_and_the_steps_done_are_published() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    pc.block_until_cancel(Call::AppStop);
    let (c, shared) = (g.cancel.clone(), Arc::clone(&g.shared));
    let seen = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        let v = shared.view();
        c.preempt();
        v
    });
    run_switch(&mut pc, &mut g, Mode::Event, Mode::Dev);
    let v = seen.join().unwrap();
    assert_eq!(v.running, Some(Mode::Dev));
    assert_eq!(
        v.switching.map(|s| (s.from, s.to, s.done)),
        Some((Mode::Event, Mode::Dev, vec![Step::Precheck]))
    );
    assert!(g.alarms.all().is_empty(), "{:?}", texts(&g));
    assert_eq!(g.shared.view().running, None);
}

#[test]
fn a_parked_engine_at_ide_event_is_no_success() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    pc.fail(Call::EngineStop, "no DriverReleased within 10 s");
    pc.health(Health::Parked);
    let r = handle(&mut pc, &mut g, Request::Event { dry_run: false }, 0);
    assert!(!r.ok);
    assert_eq!(r.mode, Mode::Event);
    assert!(
        r.detail
            .starts_with("event: stopped; the owner decides; the mode is event"),
        "{}",
        r.detail
    );
    assert!(r.alarms.last().unwrap().owner_question);
}

#[test]
fn a_failed_pref_check_follows_on_pref_fail() {
    for (choice, starts) in [
        (PrefFail::KeepReaperDown, false),
        (PrefFail::StartReaperWithAlarm, true),
    ] {
        let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
        g.site.on_pref_fail = choice;
        pc.fail(Call::PrefCheck, "3 restores failed");
        run_switch(&mut pc, &mut g, Mode::Dev, Mode::Event);
        assert_eq!(pc.called(Call::ReaperStart), starts, "{choice:?}");
        assert!(g.alarms.iter().any(|a| a.step == Some(Step::PrefCheck)));
    }
}

// ---- the switch runner ----

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
    let r = handle(&mut pc, &mut g, Request::Event { dry_run: false }, 0);
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
    let r = handle(&mut pc, &mut g, dev(), 0);
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
    let r = handle(&mut pc, &mut g, dev(), 0);
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
    let r = handle(&mut pc, &mut g, dev(), 0);
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

/// What the event plan's check says of REAPER holding the card at 32.
const HELD: &str =
    "REAPER runs with the preferred buffer at 32; it is restored at REAPER's next start";

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
        let r = ask(&mut pc, &mut g, Request::Event { dry_run: false });
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
    assert!(ask(&mut pc, &mut g, Request::Event { dry_run: false }).ok);
    let r = ask(&mut pc, &mut g, Request::Event { dry_run: false });
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
    let r = ask(&mut pc, &mut g, Request::Event { dry_run: false });
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
    assert!(ask(&mut pc, &mut g, Request::Event { dry_run: false }).ok);
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
    let r = ask(&mut pc, &mut g, Request::Event { dry_run: false });
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
    let r = handle(&mut pc, &mut g, dev(), 0);
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
}

#[test]
fn a_failed_arm_readiness_or_identity_unwinds() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    // No active bundle: the identity check cannot name one.
    let r = handle(&mut pc, &mut g, dev(), 0);
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
    let r = handle(&mut pc, &mut g, dev(), 0);
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
        let r = handle(&mut pc, &mut g, dev(), 0);
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

/// Pre-empts from another thread 50 ms after `step` was published as done.
fn preempt_after(g: &Guard, step: Step) -> std::thread::JoinHandle<()> {
    let (c, shared) = (g.cancel.clone(), Arc::clone(&g.shared));
    std::thread::spawn(move || {
        let t = Instant::now();
        while !shared
            .view()
            .switching
            .is_some_and(|s| s.done.contains(&step))
        {
            assert!(t.elapsed() < Duration::from_secs(5), "{step:?} never done");
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(50));
        c.preempt();
    })
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
    let r = handle(&mut pc, &mut g, Request::RehearseTeardown, 0);
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
    // A pre-emption during an event plan ends only that wait: the plan goes on.
    let out = run_switch(&mut pc, &mut g, Mode::Event, Mode::Event);
    fired.join().unwrap();
    assert!(t.elapsed() >= Duration::from_millis(200));
    assert_eq!(out, Outcome::Done);
    assert_eq!(
        g.alarms.all()[0].text,
        "ReaperHandover: pre-empted inside the event plan"
    );
    assert!(pc.called(Call::AppAnswers) && pc.called(Call::Fingerprint));
}

#[test]
fn an_event_unwind_does_not_recurse_when_a_step_is_preempted() {
    // A bounded, deterministic catch for the `to != Mode::Event` -> `true`
    // mutant of the preempted-step arm (daemon.rs:869). In an event plan
    // (to == Event) the real code treats a preempted step as an ordinary
    // event-plan failure and goes on, so `run_switch` returns Done after a
    // single ReaperFacts read. The mutant makes the guard `true`, so the
    // preempted ReaperHandover step sends the event switch to `back_to_event`,
    // which re-runs the whole event plan; that re-run's ReaperHandover then
    // blocks on ReaperFacts for `BLOCK_LIMIT` (10 s), the token never renewed.
    // Run it on a thread and require it to end within a bound: the original
    // returns in well under a second with one ReaperFacts read; the mutant does
    // not, so this fails its assertion in 3 s (a clean FAIL) instead of hanging
    // until nextest's slow-timeout (#23, and it runs first under
    // `priority = 100`). The `(Done, 1)` shape also fails the mutant's second
    // ReaperFacts read should `BLOCK_LIMIT` ever drop below the bound.
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut pc = FakePc::new(band_up());
        // The handover step waits on the token; a preemption ends its wait.
        pc.block_until_cancel(Call::ReaperFacts);
        let mut g = Guard::for_test(Mode::Event);
        // Fire the single preemption only after PrefCheck (the step right
        // before ReaperHandover in the event plan) is published as done, i.e.
        // after `begin` cleared the token at the plan's start: a fixed delay
        // could race a slow start and be erased by that clear.
        let _fired = preempt_after(&g, Step::PrefCheck);
        let out = run_switch(&mut pc, &mut g, Mode::Event, Mode::Event);
        let _ = done_tx.send((out, pc.count(Call::ReaperFacts)));
    });
    assert_eq!(
        done_rx.recv_timeout(Duration::from_secs(3)),
        Ok((Outcome::Done, 1)),
        "an event switch recursed into back_to_event on a preempted step \
         instead of continuing the plan after one handover (or did not end \
         within 3 s)"
    );
}

#[test]
fn event_plan_failures_follow_the_policy() {
    // A failed app stop skips the app start; the handover still runs.
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
        Outcome::Done
    );
    assert!(pc.called(Call::AppStop) && !pc.called(Call::AppStart));
    assert!(pc.called(Call::AppAnswers) && pc.called(Call::Fingerprint));
    assert_eq!(texts(&g), ["AppStop: the app did not exit within 30 s"]);
    // A REAPER handover that fails alarms and goes on to the app.
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Dev));
    pc.reaper.dialogs = vec!["Save changes?".into()];
    pc.reaper.heartbeat_advanced = false;
    assert_eq!(
        run_switch(&mut pc, &mut g, Mode::Dev, Mode::Event),
        Outcome::Done
    );
    assert!(pc.called(Call::AppStart));
    assert_eq!(
        texts(&g),
        ["ReaperHandover: a REAPER dialog is open; the meter heartbeat does not advance"]
    );
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
    let r = handle(&mut pc, &mut g, Request::Event { dry_run: false }, 0);
    assert!(r.ok, "{r:?}");
    assert_eq!(
        r.detail,
        "event: done; tuning exit: exit: ok; the preferred buffer was restored (2 writes); \
         UNCONFIRMED-AUDIO: every stage input at the meter floor"
    );
    // A preference already at the original is not reported.
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    let r = handle(&mut pc, &mut g, Request::Event { dry_run: false }, 0);
    assert_eq!(r.detail, "event: done; tuning exit: exit: ok");
}

/// REAPER's evaluation notice as the first guard start on the PC saw it
/// (#9, 2026-09-28): REAPER shows it at every start and runs normally with
/// it open.
const NOTICE: &str = "About REAPER v7.65/win64 rev 0a1b2c";

/// What `iemmode status` gets: the view's reply, answered by the pipe.
fn status_reply(g: &Guard) -> String {
    match g.shared.route(&Request::Status) {
        Route::Now(r) => r.detail,
        other => panic!("{other:?}"),
    }
}

#[test]
fn reapers_evaluation_notice_is_reported_and_never_an_alarm() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    pc.reaper.dialogs = vec![NOTICE.into()];
    let r = ask(&mut pc, &mut g, Request::Event { dry_run: false });
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
    let r = ask(&mut pc, &mut g, Request::Event { dry_run: false });
    assert_eq!(texts(&g), ["ReaperHandover: a REAPER dialog is open"]);
    assert!(
        r.detail.contains(r#"; reaper_notice: "evaluation""#),
        "{}",
        r.detail
    );
    // Without it the report and the status do not name it.
    pc.reaper.dialogs.clear();
    let r = ask(&mut pc, &mut g, Request::Event { dry_run: false });
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
    assert!(ask(&mut pc, &mut g, Request::Event { dry_run: false }).ok);
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
    assert!(ask(&mut pc, &mut g, Request::Event { dry_run: false }).ok);
    pc.fail(Call::ReaperFacts, "REAPER's windows: access denied");
    let r = ask(&mut pc, &mut g, Request::Event { dry_run: false });
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
    let r = handle(&mut pc, &mut g, Request::Event { dry_run: false }, 0);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.job, None);
    assert!(r.detail.contains("HIL job 4242 cancelled"), "{}", r.detail);
    assert!(pc.index(Call::RunnerStop) < pc.index(Call::EngineStop));
}

// ---- requests ----

#[test]
fn jobs_are_refused_while_switching() {
    let shared = Shared::new(Cancel::default());
    shared.update(|v| {
        v.running = Some(Mode::Dev);
        v.status = "mode event".into();
    });
    let refused = [
        Request::JobBegin { run: 1 },
        Request::JobEnd { run: 1 },
        Request::Install {
            zip: "b.zip".into(),
        },
        Request::Activate { sha: SHA.into() },
        Request::TestSignal {
            input: "mic1".into(),
            dbfs: -30.0,
            ttl_s: 5.0,
        },
        Request::Report {
            sha: SHA.into(),
            hil: "green".into(),
            detail: "x".into(),
        },
        Request::InstallSite {
            path: "site.toml".into(),
        },
        Request::ForceReopen,
        Request::InjectFault,
        Request::InjectSeh,
        Request::InjectPark,
        Request::RunnerStop,
        Request::ProbeTask,
        Request::RehearseTeardown,
        Request::AlarmTest,
        Request::AlarmAck { id: 1 },
        Request::Quit,
        Request::Event { dry_run: true },
    ];
    for req in &refused {
        match shared.route(req) {
            Route::Now(r) => assert_eq!((r.ok, r.detail.as_str()), (false, "switching"), "{req:?}"),
            other => panic!("{req:?}: {other:?}"),
        }
    }
    let live = Request::Live {
        build: SHA.into(),
        trial: false,
        dry_run: false,
    };
    for req in [dev(), live] {
        match shared.route(&req) {
            Route::Now(r) => assert_eq!((r.ok, r.detail.as_str()), (false, "busy")),
            other => panic!("{other:?}"),
        }
    }
    match shared.route(&Request::Status) {
        Route::Now(r) => assert_eq!((r.ok, r.detail.as_str()), (true, "mode event")),
        other => panic!("{other:?}"),
    }
    assert_eq!(shared.route(&Request::Subscribe), Route::Subscribe);
    // A job queued before a switch began is answered as during it.
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    let epoch = g.shared.epoch();
    g.shared.update(|v| v.epoch += 1);
    let r = handle(&mut pc, &mut g, Request::JobBegin { run: 4242 }, epoch);
    assert_eq!((r.ok, r.detail.as_str()), (false, "switching"));
    assert_eq!(g.state.job, None);
    let r = handle(&mut pc, &mut g, dev(), epoch);
    assert_eq!((r.ok, r.detail.as_str()), (false, "busy"));
    assert!(pc.calls().is_empty());
    let r = handle(&mut pc, &mut g, Request::Event { dry_run: false }, epoch);
    assert!(!r.ok);
    assert_eq!(r.detail, "a switch ran meanwhile; event: no switch yet");
    assert!(pc.calls().is_empty());
    let r = handle(&mut pc, &mut g, Request::Status, epoch);
    assert!(r.ok);
    // The same generation is handled.
    let r = handle(&mut pc, &mut g, Request::JobBegin { run: 4242 }, epoch + 1);
    assert!(r.ok, "{r:?}");
}

#[test]
fn ide_event_preempts_or_waits_but_never_both() {
    let cancel = Cancel::default();
    let shared = Shared::new(cancel.clone());
    // Idle: pre-empt whatever is queued ahead, then queue.
    assert_eq!(
        shared.route(&Request::Event { dry_run: false }),
        Route::Queue(0)
    );
    assert!(cancel.preempted());
    cancel.clear();
    // During an event plan: wait for it, the token untouched.
    shared.update(|v| {
        v.running = Some(Mode::Event);
        v.epoch = 3;
    });
    assert_eq!(
        shared.route(&Request::Event { dry_run: false }),
        Route::AwaitEnd("already switching to event")
    );
    assert!(!cancel.preempted());
    // During any other switch: pre-empt it and wait.
    shared.update(|v| v.running = Some(Mode::Live));
    assert_eq!(
        shared.route(&Request::Event { dry_run: false }),
        Route::AwaitEnd("pre-empted the switch in progress")
    );
    assert!(cancel.preempted());
    // Other requests while idle go to the daemon with the generation.
    shared.update(|v| v.running = None);
    assert_eq!(shared.route(&Request::Quit), Route::Queue(3));
    assert_eq!(
        shared.route(&Request::Event { dry_run: true }),
        Route::Queue(3)
    );
}

#[test]
fn await_end_answers_when_the_switch_ended() {
    let shared = Arc::new(Shared::new(Cancel::default()));
    shared.update(|v| v.running = Some(Mode::Event));
    let other = Arc::clone(&shared);
    let t = Instant::now();
    let ender = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        other.update(|v| {
            v.running = None;
            v.mode = Mode::Event;
            v.last = Some(Outcome::Done);
        });
    });
    let r = shared.await_end("already switching to event", Duration::from_secs(5));
    ender.join().unwrap();
    assert!(t.elapsed() >= Duration::from_millis(200));
    assert!(t.elapsed() < Duration::from_secs(3));
    assert!(r.ok);
    assert_eq!(r.detail, "already switching to event; event: done");
    // A switch that does not end: not done after the limit.
    shared.update(|v| v.running = Some(Mode::Event));
    let t = Instant::now();
    let r = shared.await_end("waited", Duration::from_millis(100));
    assert!(t.elapsed() >= Duration::from_millis(100));
    assert!(!r.ok);
    // Ended, but not in event, or not done.
    for (mode, last) in [
        (Mode::Dev, Some(Outcome::Done)),
        (Mode::Event, Some(Outcome::NeedsOwner)),
        (Mode::Event, None),
    ] {
        shared.update(|v| {
            v.running = None;
            v.mode = mode;
            v.last = last;
        });
        assert!(
            !shared.await_end("x", Duration::ZERO).ok,
            "{mode:?} {last:?}"
        );
    }
}

#[test]
fn a_tray_quit_waits_for_the_next_subscriber() {
    let shared = Shared::new(Cancel::default());
    // Without a subscriber the quit waits: the tray may be in its 2 s retry.
    assert_eq!(shared.tray_quit(), Ok(()));
    assert!(shared.take_tray_quit());
    assert!(!shared.take_tray_quit(), "a quit is delivered once");
    shared.add_subscriber();
    assert_eq!(shared.tray_quit(), Ok(()));
    assert_eq!(shared.view().tray_quits, 2);
    // It wakes the subscriber at once.
    let seen = (shared.view().version, 1);
    let t = Instant::now();
    let v = shared.wait_change(seen, Duration::from_secs(5));
    assert!(t.elapsed() < Duration::from_secs(1));
    assert_eq!(v.tray_quits, 2);
    // A tray start forgets a quit no tray took.
    shared.clear_tray_quit();
    assert!(!shared.take_tray_quit());
    shared.drop_subscriber();
    shared.drop_subscriber();
    assert_eq!(shared.view().subscribers, 0);
    // No change: the wait ends at its limit.
    let v = shared.view();
    let t = Instant::now();
    shared.wait_change((v.version, v.tray_quits), Duration::from_millis(100));
    assert!(t.elapsed() >= Duration::from_millis(100));
}

#[test]
fn a_tray_start_forgets_a_quit_no_tray_took() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    // A stop whose tray never came back to take its quit.
    g.shared.tray_quit().unwrap();
    run_switch(&mut pc, &mut g, Mode::Event, Mode::Dev);
    assert!(pc.called(Call::TrayStart));
    assert_eq!(g.state.mode, Mode::Dev);
    assert!(
        !g.shared.take_tray_quit(),
        "the new tray must not quit at once"
    );
}

#[test]
fn dry_run_changes_nothing() {
    let live = Request::Live {
        build: SHA.into(),
        trial: false,
        dry_run: true,
    };
    let dry_dev = Request::Dev {
        build: None,
        dry_run: true,
    };
    for req in [Request::Event { dry_run: true }, dry_dev, live] {
        let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
        g.state
            .bundles
            .insert(SHA.into(), record(SHA, "main", Hil::Green));
        let r = handle(&mut pc, &mut g, req.clone(), 0);
        assert!(r.ok, "{req:?}: {r:?}");
        assert!(r.detail.starts_with("dry run: "), "{}", r.detail);
        assert_eq!(pc.mutating_calls(), Vec::<Call>::new(), "{req:?}");
        assert_eq!(g.state.mode, Mode::Event);
        assert_eq!(g.state.pins, Pins::default());
        assert_eq!(g.shared.view().epoch, 0);
    }
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    let r = handle(
        &mut pc,
        &mut g,
        Request::Dev {
            build: None,
            dry_run: true,
        },
        0,
    );
    assert_eq!(
        r.detail,
        "dry run: Precheck, AppStop, ReaperSaveQuit, TuningEnter, Data, PrefCheck, EngineStart, \
         EngineArm, ServerStart, TrayStart, IdentityCheck, RunnerStart; bundle none; precheck ok"
    );
    pc.fail(Call::Precheck, "an engine the guard did not start runs");
    let r = handle(
        &mut pc,
        &mut g,
        Request::Dev {
            build: None,
            dry_run: true,
        },
        0,
    );
    assert!(!r.ok);
    assert!(
        r.detail
            .ends_with("RunnerStart; bundle none; precheck an engine the guard did not start runs"),
        "{}",
        r.detail
    );
    let r = handle(&mut pc, &mut g, Request::Event { dry_run: true }, 0);
    assert_eq!(
        r.detail,
        "dry run: TuningExit, PrefCheck, ReaperHandover, AppHandover, Fingerprint"
    );
}

/// The precheck's text for a missing PWA notification subscription.
const NO_SUBSCRIPTION: &str =
    "no PWA notification subscription: no engineer device allowed notifications";
/// Its dev note (`pc::precheck`).
const NO_SUBSCRIPTION_NOTE: &str = "no PWA notification subscription: no engineer device \
     allowed notifications (not needed for dev: the alarms stay in the guard's alarm file)";

/// The alarms go to the engineer's PWA notification subscriptions (#9
/// 2026-09-28); the predecessor's arrive with the band import, a later step
/// of the entry, and a new one only through iem-server (dev and live): a
/// dev entry without one goes on and names it, in its report and in the
/// status until an alarm reaches a phone.
#[test]
fn a_dev_entry_without_a_pwa_subscription_goes_on_and_names_it() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    pc.subscriptions = Some(0);
    let r = ask(&mut pc, &mut g, dev());
    assert!(r.ok, "{r:?}");
    assert!(
        r.detail.starts_with(&format!(
            "dev: done; {NO_SUBSCRIPTION_NOTE}; tuning enter: "
        )),
        "{}",
        r.detail
    );
    assert_eq!(g.state.mode, Mode::Dev);
    assert!(g.alarms.all().is_empty(), "{:?}", texts(&g));
    assert_eq!(
        status_reply(&g),
        format!("mode dev; bundle {SHA}; {NO_SUBSCRIPTION_NOTE}")
    );
    assert_eq!(
        ask(&mut pc, &mut g, Request::Status).detail,
        format!("mode dev; bundle {SHA}; {NO_SUBSCRIPTION_NOTE}")
    );
    // Its dry run passes and names it too.
    let dry = Request::Dev {
        build: None,
        dry_run: true,
    };
    let r = ask(&mut pc, &mut g, dry);
    assert!(r.ok, "{r:?}");
    assert!(
        r.detail.ends_with(&format!(
            "; bundle {SHA}; precheck ok; {NO_SUBSCRIPTION_NOTE}"
        )),
        "{}",
        r.detail
    );
    // An alarm that does not reach a phone keeps it named.
    pc.fail(Call::Notify, "no device took the notice");
    let r = ask(&mut pc, &mut g, Request::AlarmTest);
    assert!(!r.ok, "{r:?}");
    assert!(
        status_reply(&g).contains(NO_SUBSCRIPTION_NOTE),
        "{}",
        status_reply(&g)
    );
    // The engineer allows notifications in the mixer app (iem-server now
    // serves it) and the alarm test reaches the phone: the status no
    // longer names it.
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    pc.subscriptions = Some(0);
    assert!(ask(&mut pc, &mut g, dev()).ok);
    pc.subscriptions = Some(1);
    let r = ask(&mut pc, &mut g, Request::AlarmTest);
    assert!(r.ok, "{r:?}");
    assert!(
        !status_reply(&g).contains(NO_SUBSCRIPTION),
        "{}",
        status_reply(&g)
    );
    // With a subscription the entry names nothing.
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    let r = ask(&mut pc, &mut g, dev());
    assert!(r.ok, "{r:?}");
    assert!(!r.detail.contains("PWA notification"), "{}", r.detail);
    assert_eq!(status_reply(&g), format!("mode dev; bundle {SHA}"));
}

/// What `tls::check` names on the PC (#9 2026-09-28): LAN 443 serves the
/// certificate `iem-migrate band` took over, and the predecessor serves the
/// same one, expired months ago.
const LAN_NOTE: &str =
    "the LAN certificate expired on 2026-03-01; the predecessor serves the same one";

/// The identity check proves identity, not validity: a certificate outside
/// its validity is named once in the entry's report and in the status
/// until the next check, never an alarm.
#[test]
fn an_expired_lan_certificate_is_named_and_never_an_alarm() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    pc.lan_note = Some(LAN_NOTE.into());
    let r = ask(&mut pc, &mut g, dev());
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Dev);
    assert_eq!(r.detail.matches(LAN_NOTE).count(), 1, "{}", r.detail);
    assert!(r.detail.contains(&format!("; {LAN_NOTE}")), "{}", r.detail);
    // Reported where the check ran: after the server started.
    assert!(
        r.detail.find("server started") < r.detail.find(LAN_NOTE),
        "{}",
        r.detail
    );
    assert!(g.alarms.all().is_empty(), "{:?}", texts(&g));
    assert_eq!(
        status_reply(&g),
        format!("mode dev; bundle {SHA}; {LAN_NOTE}")
    );
    assert_eq!(
        ask(&mut pc, &mut g, Request::Status).detail,
        format!("mode dev; bundle {SHA}; {LAN_NOTE}")
    );
    // Back in event the predecessor serves the same certificate: still
    // named.
    assert!(ask(&mut pc, &mut g, Request::Event { dry_run: false }).ok);
    assert!(status_reply(&g).contains(LAN_NOTE), "{}", status_reply(&g));
    // The next check that names nothing drops it.
    pc.lan_note = None;
    let r = ask(&mut pc, &mut g, dev());
    assert!(r.ok, "{r:?}");
    assert!(!r.detail.contains("LAN certificate"), "{}", r.detail);
    assert_eq!(status_reply(&g), format!("mode dev; bundle {SHA}"));
    // A failed check does not repeat an old note.
    pc.lan_note = Some(LAN_NOTE.into());
    assert!(ask(&mut pc, &mut g, Request::Event { dry_run: false }).ok);
    assert!(ask(&mut pc, &mut g, dev()).ok);
    assert!(status_reply(&g).contains(LAN_NOTE), "{}", status_reply(&g));
    assert!(ask(&mut pc, &mut g, Request::Event { dry_run: false }).ok);
    pc.fail(Call::Identity, "LAN 443: serves another certificate");
    let r = ask(&mut pc, &mut g, dev());
    assert!(!r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Event);
    assert!(!r.detail.contains(LAN_NOTE), "{}", r.detail);
    assert!(
        !status_reply(&g).contains("LAN certificate"),
        "{}",
        status_reply(&g)
    );
}

#[test]
fn a_live_entry_without_a_pwa_subscription_is_refused() {
    let live = |trial, dry_run| Request::Live {
        build: SHA.into(),
        trial,
        dry_run,
    };
    for trial in [false, true] {
        let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
        g.state
            .bundles
            .insert(SHA.into(), record(SHA, "main", Hil::Green));
        pc.subscriptions = Some(0);
        let r = ask(&mut pc, &mut g, live(trial, true));
        assert!(!r.ok, "{r:?}");
        assert!(
            r.detail.ends_with(&format!("precheck {NO_SUBSCRIPTION}")),
            "{}",
            r.detail
        );
        let r = ask(&mut pc, &mut g, live(trial, false));
        assert!(!r.ok, "{r:?}");
        assert_eq!(g.state.mode, Mode::Event);
        assert!(!pc.called(Call::EngineStart));
        assert_eq!(texts(&g), [format!("Precheck: {NO_SUBSCRIPTION}")]);
    }
}

/// The precheck's text for a trial before the PC tests (`pc::precheck`).
const NO_PC_TESTS: &str =
    "[guard] pc_tests_passed is false: no trial before the owner-approved PC tests";

/// `live --trial` waits for the owner-approved PC tests (design §10,
/// `[guard] pc_tests_passed`): its entry and its dry run are refused
/// without them and pass with them, and a live entry that is no trial is
/// not affected. The trial reaches the precheck only through the entry
/// (`g.trial = e.trial`, the dry run's `e.trial`; review of PR #39, #38):
/// dropping either passes a trial here.
#[test]
fn a_live_trial_needs_the_pc_tests_and_a_plain_live_entry_does_not() {
    let live = |trial, dry_run| Request::Live {
        build: SHA.into(),
        trial,
        dry_run,
    };
    let fresh = |pc_tests_passed| {
        let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
        g.state
            .bundles
            .insert(SHA.into(), record(SHA, "main", Hil::Green));
        pc.pc_tests_passed = pc_tests_passed;
        (pc, g)
    };
    // Without the PC tests: the trial's dry run and its entry are refused.
    let (mut pc, mut g) = fresh(false);
    let r = ask(&mut pc, &mut g, live(true, true));
    assert!(!r.ok, "{r:?}");
    assert!(
        r.detail.ends_with(&format!("precheck {NO_PC_TESTS}")),
        "{}",
        r.detail
    );
    assert_eq!(pc.mutating_calls(), Vec::<Call>::new());
    let r = ask(&mut pc, &mut g, live(true, false));
    assert!(!r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Event);
    assert!(!pc.called(Call::EngineStart));
    assert_eq!(texts(&g), [format!("Precheck: {NO_PC_TESTS}")]);
    // With them the same trial passes both.
    let (mut pc, mut g) = fresh(true);
    let r = ask(&mut pc, &mut g, live(true, true));
    assert!(r.ok, "{r:?}");
    assert!(r.detail.ends_with("; precheck ok"), "{}", r.detail);
    let r = ask(&mut pc, &mut g, live(true, false));
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Live);
    // A live entry that is no trial does not read them.
    let (mut pc, mut g) = fresh(false);
    let r = ask(&mut pc, &mut g, live(false, true));
    assert!(r.ok, "{r:?}");
    assert!(r.detail.ends_with("; precheck ok"), "{}", r.detail);
    let r = ask(&mut pc, &mut g, live(false, false));
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Live);
}

#[test]
fn live_needs_an_installed_green_main_bundle() {
    let live = |trial| Request::Live {
        build: SHA.into(),
        trial,
        dry_run: false,
    };
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    let r = handle(&mut pc, &mut g, live(false), 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, format!("bundle {SHA} is not installed").as_str())
    );
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "dev", Hil::Green));
    let r = handle(&mut pc, &mut g, live(false), 0);
    assert_eq!(r.detail, format!("{SHA} is from \"dev\"; live needs main"));
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Pending));
    let r = handle(&mut pc, &mut g, live(false), 0);
    assert_eq!(r.detail, format!("{SHA}: HIL Pending; live needs green"));
    assert!(pc.calls().is_empty());
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Green));
    let r = handle(&mut pc, &mut g, live(false), 0);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Live);
    assert_eq!(g.state.pins.current.as_deref(), Some(SHA));
    assert_eq!(pc.bundle.as_deref(), Some(SHA));
    assert!(!pc.called(Call::RunnerStart));
    // A trial (the band there on purpose) enters the same way.
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Green));
    let r = handle(&mut pc, &mut g, live(true), 0);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Live);
}

#[test]
fn dev_with_a_build_pins_it() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    let build = |sha: &str| Request::Dev {
        build: Some(sha.into()),
        dry_run: false,
    };
    let r = handle(&mut pc, &mut g, build(SHA), 0);
    assert_eq!(r.detail, format!("bundle {SHA} is not installed"));
    g.state.pins.current = Some(OTHER.into());
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "dev", Hil::Pending));
    let r = handle(&mut pc, &mut g, build(SHA), 0);
    assert!(r.ok, "{r:?}");
    assert_eq!(
        g.state.pins,
        Pins {
            current: Some(SHA.into()),
            previous: Some(OTHER.into()),
        }
    );
    assert_eq!(pc.bundle.as_deref(), Some(SHA));
}

/// #38 (owner, 2026-10-06): a HIL job begins in dev when no other job runs,
/// whatever the stage carries. Nothing reads it: other devices on the Dante
/// network feed the card's inputs, and only the owner's signal decides
/// whether the PC may be used.
#[test]
fn a_hil_job_begins_in_dev_without_reading_the_stage() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    assert_eq!(
        handle(&mut pc, &mut g, Request::JobBegin { run: 7 }, 0),
        g.reply(true, "HIL job 7 began")
    );
    assert_eq!(g.state.job, Some(7));
    assert_eq!(pc.calls(), Vec::<Call>::new(), "no PC read");
}

/// #38: a dev entry reads no stage, never waits for quiet and never refuses
/// on activity: from the band's system the app's stop follows the precheck,
/// and with nothing running the tuning enter does.
#[test]
fn a_dev_entry_reads_no_stage_and_never_waits_for_quiet() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    let r = handle(&mut pc, &mut g, dev(), 0);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Dev);
    assert_eq!(
        steps(&pc).get(..3),
        Some(&[Call::Precheck, Call::AppStop, Call::ReaperSaveQuit][..])
    );
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    let r = handle(&mut pc, &mut g, dev(), 0);
    assert!(r.ok, "{r:?}");
    assert_eq!(
        steps(&pc).get(..2),
        Some(&[Call::Precheck, Call::Tuning][..])
    );
}

#[test]
fn a_job_begins_once_and_ends_by_its_run() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    let begin = Request::JobBegin { run: 7 };
    let r = handle(&mut pc, &mut g, begin.clone(), 0);
    assert_eq!((r.ok, r.detail.as_str()), (true, "HIL job 7 began"));
    assert_eq!(g.state.job, Some(7));
    let r = handle(&mut pc, &mut g, Request::JobBegin { run: 8 }, 0);
    assert_eq!(r.detail, "HIL job 7 has not ended");
    // job-end
    let r = handle(&mut pc, &mut g, Request::JobEnd { run: 8 }, 0);
    assert_eq!((r.ok, r.detail.as_str()), (false, "HIL job 7 runs, not 8"));
    let r = handle(&mut pc, &mut g, Request::JobEnd { run: 7 }, 0);
    assert_eq!((r.ok, r.detail.as_str()), (true, "HIL job 7 ended"));
    assert_eq!(g.state.job, None);
    let r = handle(&mut pc, &mut g, Request::JobEnd { run: 7 }, 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (true, "no HIL job runs (job 7 has ended)")
    );
    // Only in dev.
    g.state.mode = Mode::Live;
    let r = handle(&mut pc, &mut g, begin, 0);
    assert_eq!(r.detail, "a HIL job is for dev; the mode is live");
}

#[test]
fn the_test_signal_goes_only_to_the_hil_outputs() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    g.state.job = Some(7);
    let signal = |dbfs: f64, ttl_s: f64| Request::TestSignal {
        input: "mic1".into(),
        dbfs,
        ttl_s,
    };
    let r = handle(&mut pc, &mut g, signal(-20.0, 5.0), 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (
            true,
            "test signal on mic1 at -20 dBFS for 5 s on card outputs [94]"
        )
    );
    assert_eq!(pc.hil_signals, [("mic1".to_owned(), -20.0, 5.0, vec![94])]);
    for (dbfs, ttl, why) in [
        (
            -19.9,
            5.0,
            "-19.9 dBFS is above the HIL ceiling of -20 dBFS",
        ),
        (
            f64::NAN,
            5.0,
            "NaN dBFS is above the HIL ceiling of -20 dBFS",
        ),
        (-30.0, 0.0, "a TTL of 0 s is not a positive time"),
        (-30.0, -1.0, "a TTL of -1 s is not a positive time"),
        (
            -30.0,
            f64::INFINITY,
            "a TTL of inf s is not a positive time",
        ),
        (-30.0, f64::NAN, "a TTL of NaN s is not a positive time"),
    ] {
        let r = handle(&mut pc, &mut g, signal(dbfs, ttl), 0);
        assert_eq!((r.ok, r.detail.as_str()), (false, why));
    }
    assert_eq!(pc.hil_signals.len(), 1);
    pc.fail(Call::HilSignal, "not a supervisor");
    let r = handle(&mut pc, &mut g, signal(-30.0, 1.0), 0);
    assert_eq!((r.ok, r.detail.as_str()), (false, "not a supervisor"));
    g.site.hil_tx.clear();
    let r = handle(&mut pc, &mut g, signal(-30.0, 1.0), 0);
    assert_eq!(
        r.detail,
        "[guard] hil_tx is empty: no card output may carry a test signal"
    );
    g.state.mode = Mode::Event;
    let r = handle(&mut pc, &mut g, signal(-30.0, 1.0), 0);
    assert_eq!(r.detail, "test-signal is for dev; the mode is event");
    assert_eq!(HIL_MAX_DBFS, -20.0);
}

#[test]
fn a_test_signal_needs_a_begun_job_and_at_most_60_s() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    let signal = |ttl_s: f64| Request::TestSignal {
        input: "mic1".into(),
        dbfs: -30.0,
        ttl_s,
    };
    // Without a begun job: refused.
    let r = handle(&mut pc, &mut g, signal(5.0), 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "a test signal needs a begun HIL job (job-begin)")
    );
    assert!(!pc.called(Call::HilSignal));
    g.state.job = Some(7);
    let r = handle(&mut pc, &mut g, signal(60.0), 0);
    assert!(r.ok, "{r:?}");
    let r = handle(&mut pc, &mut g, signal(60.5), 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "a TTL of 60.5 s is above the HIL limit of 60 s")
    );
    assert_eq!(pc.hil_signals, [("mic1".to_owned(), -30.0, 60.0, vec![94])]);
    assert_eq!(HIL_MAX_TTL_S, 60.0);
}

#[test]
fn a_hil_report_records_the_result() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Dev));
    let report = |hil: &str| Request::Report {
        sha: SHA.into(),
        hil: hil.into(),
        detail: "120 s at 32".into(),
    };
    let r = handle(&mut pc, &mut g, report("green"), 0);
    assert_eq!(r.detail, format!("bundle {SHA} is not installed"));
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Pending));
    let r = handle(&mut pc, &mut g, report("green"), 0);
    assert_eq!(
        (r.ok, r.detail.clone()),
        (true, format!("bundle {SHA}: HIL green (120 s at 32)"))
    );
    assert_eq!(g.state.bundles[SHA].hil, Hil::Green);
    handle(&mut pc, &mut g, report("red"), 0);
    assert_eq!(g.state.bundles[SHA].hil, Hil::Red);
    let r = handle(&mut pc, &mut g, report("yellow"), 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "HIL result \"yellow\": green or red")
    );
    assert_eq!(g.state.bundles[SHA].hil, Hil::Red);
}

#[test]
fn dev_only_requests_are_refused_elsewhere() {
    for (req, what) in [
        (Request::ForceReopen, "force-reopen"),
        (Request::InjectFault, "inject-fault"),
        (Request::InjectSeh, "inject-seh"),
        (Request::InjectPark, "inject-park"),
        (Request::RunnerStop, "runner-stop"),
        (Request::RehearseTeardown, "rehearse-teardown"),
        (
            Request::InstallSite {
                path: "site.toml".into(),
            },
            "install-site",
        ),
    ] {
        let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
        let r = handle(&mut pc, &mut g, req, 0);
        assert_eq!(
            (r.ok, r.detail),
            (false, format!("{what} is for dev; the mode is event"))
        );
        assert!(pc.mutating_calls().is_empty(), "{what}");
    }
}

#[test]
fn engine_and_runner_requests_in_dev() {
    let (mut pc, mut g) = (
        FakePc::new(Facts {
            runner: true,
            ..iemmixer_up()
        }),
        Guard::for_test(Mode::Dev),
    );
    let r = handle(&mut pc, &mut g, Request::ForceReopen, 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (true, "the engine reopened the driver")
    );
    g.state.job = Some(3);
    let r = handle(&mut pc, &mut g, Request::RunnerStop, 0);
    assert_eq!(r.detail, "HIL job 3 runs: the runner is not idle");
    assert!(!pc.called(Call::RunnerStop));
    g.state.job = None;
    let r = handle(&mut pc, &mut g, Request::RunnerStop, 0);
    assert_eq!((r.ok, r.detail.as_str()), (true, "the runner stopped"));
    pc.fail(Call::ForceReopen, "the reset budget is spent");
    let r = handle(&mut pc, &mut g, Request::ForceReopen, 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "the reset budget is spent")
    );
    // The probe task runs in any mode.
    g.state.mode = Mode::Event;
    let r = handle(&mut pc, &mut g, Request::ProbeTask, 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (true, "the probe task ended with 0")
    );
}

/// A rehearsal ends by entering dev again, and a dev entry cancels the jobs
/// and stops the runner: inside a HIL job that would stop the runner that
/// runs the job, so it is refused before any step, as runner-stop is.
#[test]
fn rehearse_teardown_is_refused_inside_a_hil_job() {
    let (mut pc, mut g) = (
        FakePc::new(Facts {
            runner: true,
            ..iemmixer_up()
        }),
        Guard::for_test(Mode::Dev),
    );
    g.state.pins.current = Some(SHA.into());
    g.state.job = Some(3);
    let r = handle(&mut pc, &mut g, Request::RehearseTeardown, 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (
            false,
            "HIL job 3 runs: the rehearsal's dev entry would stop its runner"
        )
    );
    assert!(pc.mutating_calls().is_empty(), "{:?}", pc.mutating_calls());
    assert_eq!((g.state.job, g.state.mode), (Some(3), Mode::Dev));
    assert!(g.alarms.all().is_empty(), "{:?}", texts(&g));
    // After the job the rehearsal runs.
    g.state.job = None;
    let r = handle(&mut pc, &mut g, Request::RehearseTeardown, 0);
    assert!(r.ok, "{r:?}");
    assert!(pc.called(Call::RunnerStop) && pc.called(Call::EngineStop));
}

// ---- HIL: the engine in the reply, the fault, the job's restarts ----

/// The engine of `FakePc::seen` (build `2.0.0-dev.9+<SHA>`) as `Reply.engine`.
fn engine_of(spawns: u64, last_exit: Option<i32>) -> EngineStatus {
    EngineStatus {
        build: SHA.to_owned(),
        frames: 32,
        callbacks: 30_000,
        pipe_private: true,
        spawns,
        last_exit,
        ..EngineStatus::default()
    }
}

#[test]
fn replies_carry_the_running_engine() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Dev));
    let r = handle(&mut pc, &mut g, Request::Status, 0);
    assert_eq!(r.engine, None, "no engine runs");
    pc.facts.engine = true;
    pc.seen.status.missed = 1;
    pc.seen.status.resets = 2;
    let r = handle(&mut pc, &mut g, Request::Status, 0);
    assert_eq!(
        r.engine,
        Some(EngineStatus {
            missed: 1,
            resets: 2,
            ..engine_of(0, None)
        })
    );
    // A status answered by the pipe's threads shows what the watch saw.
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    tick(&mut pc, &mut g, Instant::now());
    match g.shared.route(&Request::Status) {
        Route::Now(r) => assert_eq!(r.engine, Some(engine_of(0, None))),
        other => panic!("{other:?}"),
    }
    pc.facts.engine = false;
    tick(&mut pc, &mut g, Instant::now());
    match g.shared.route(&Request::Status) {
        Route::Now(r) => assert_eq!(r.engine, None),
        other => panic!("{other:?}"),
    }
    assert_eq!(g.shared.view().engine, None);
}

/// While the engine comes up (its pipe exists before its first Status) the
/// reply has no engine, on a request and in the watch's view, rather than
/// an empty build with zero counters (HIL v1 waits for it); then it shows
/// the engine.
#[test]
fn an_engine_coming_up_is_absent_from_the_reply() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    pc.engine_up = false;
    let r = handle(&mut pc, &mut g, Request::Status, 0);
    assert_eq!(r.engine, None);
    tick(&mut pc, &mut g, Instant::now());
    match g.shared.route(&Request::Status) {
        Route::Now(r) => assert_eq!(r.engine, None),
        other => panic!("{other:?}"),
    }
    pc.engine_up = true;
    tick(&mut pc, &mut g, Instant::now());
    match g.shared.route(&Request::Status) {
        Route::Now(r) => assert_eq!(r.engine, Some(engine_of(0, None))),
        other => panic!("{other:?}"),
    }
    let r = handle(&mut pc, &mut g, Request::Status, 0);
    assert_eq!(r.engine, Some(engine_of(0, None)));
}

#[test]
fn the_engines_hil_flags_are_for_a_dev_job_only() {
    // A dev entry without a job: held, no HIL flags.
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    run_switch(&mut pc, &mut g, Mode::Event, Mode::Dev);
    assert_eq!(pc.engine_starts, [(true, false)]);
    assert_eq!(g.spawns, 1);
    // A live entry never carries them, even with a job left over.
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    g.state.job = Some(7);
    run_switch(&mut pc, &mut g, Mode::Event, Mode::Live);
    assert_eq!(pc.engine_starts, [(true, false)]);
    // A respawn in dev inside a job carries them; outside a job it does not.
    for (job, hil) in [(Some(7), true), (None, false)] {
        let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Dev));
        g.state.job = job;
        pc.exited.push((Kid::Engine, Some(70)));
        let at = Instant::now();
        tick(&mut pc, &mut g, at);
        tick(&mut pc, &mut g, at + Duration::from_secs(1));
        assert_eq!(pc.engine_starts, [(false, hil)], "{job:?}");
    }
}

#[test]
fn inject_fault_is_refused_outside_a_dev_job() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Event));
    let r = handle(&mut pc, &mut g, Request::InjectFault, 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "inject-fault is for dev; the mode is event")
    );
    g.state.mode = Mode::Dev;
    let r = handle(&mut pc, &mut g, Request::InjectFault, 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "a fault needs a begun HIL job (job-begin)")
    );
    assert!(!pc.called(Call::InjectFault));
    // Inside a job it goes to the engine, whose refusal is the answer.
    g.state.job = Some(7);
    pc.fail(
        Call::InjectFault,
        "inject_fault: the engine runs without the fault-injection flag",
    );
    let r = handle(&mut pc, &mut g, Request::InjectFault, 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (
            false,
            "inject_fault: the engine runs without the fault-injection flag"
        )
    );
    assert_eq!(pc.count(Call::InjectFault), 1);
}

#[test]
fn inject_seh_is_refused_outside_a_dev_job() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Event));
    let r = handle(&mut pc, &mut g, Request::InjectSeh, 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "inject-seh is for dev; the mode is event")
    );
    g.state.mode = Mode::Dev;
    let r = handle(&mut pc, &mut g, Request::InjectSeh, 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "an SEH test needs a begun HIL job (job-begin)")
    );
    assert!(!pc.called(Call::InjectSeh));
    // Inside a job it goes to the engine, whose refusal is the answer.
    g.state.job = Some(7);
    pc.fail(
        Call::InjectSeh,
        "inject_seh: the engine runs without the fault-injection flag",
    );
    let r = handle(&mut pc, &mut g, Request::InjectSeh, 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (
            false,
            "inject_seh: the engine runs without the fault-injection flag"
        )
    );
    assert_eq!(pc.count(Call::InjectSeh), 1);
}

/// The parked-engine test (design §10 test #2, #35) leaves the card held
/// until the engine ends, so it has the SEH test's gates: dev, inside a begun
/// HIL job. The mode decides first: live and event refuse it even with a
/// job recorded, and dev without a job refuses it too; the engine is never
/// asked. Inside a job the engine's own refusal (no fault-injection flag) is
/// the answer.
#[test]
fn inject_park_is_refused_in_live_in_event_and_outside_a_job() {
    for mode in [Mode::Live, Mode::Event] {
        let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(mode));
        g.state.job = Some(7);
        let r = handle(&mut pc, &mut g, Request::InjectPark, 0);
        assert_eq!(
            (r.ok, r.detail),
            (
                false,
                format!("inject-park is for dev; the mode is {}", mode_name(mode))
            )
        );
        assert!(!pc.called(Call::InjectPark), "{mode:?}");
    }
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    let r = handle(&mut pc, &mut g, Request::InjectPark, 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (
            false,
            "a parked-engine test needs a begun HIL job (job-begin)"
        )
    );
    assert!(!pc.called(Call::InjectPark));
    g.state.job = Some(7);
    pc.fail(
        Call::InjectPark,
        "inject_park: the engine runs without the fault-injection flag",
    );
    let r = handle(&mut pc, &mut g, Request::InjectPark, 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (
            false,
            "inject_park: the engine runs without the fault-injection flag"
        )
    );
    assert_eq!(pc.count(Call::InjectPark), 1);
}

/// Inside a HIL job in dev the parked-engine test reaches the engine (#35),
/// which keeps running with its stream parked and the card held: `iemmode
/// status` reports `parked`, and the watch starts no engine (none ended).
/// The engine runs so until it ends: test #2 ends it with an OS restart, but
/// any `Shutdown` (an "ide event", a job's restart) ends it too.
#[test]
fn an_injected_park_leaves_the_engine_running_parked() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    g.state.job = Some(7);
    let r = handle(&mut pc, &mut g, Request::InjectPark, 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (
            true,
            "the engine raises a structured exception under the test hold: its stream \
             parks with the card held and the engine keeps running until it ends \
             (test #2 ends it with an OS restart)"
        )
    );
    assert_eq!(pc.count(Call::InjectPark), 1);
    // The engine's next Status: the stream parked.
    pc.seen.status.parked = true;
    let at = Instant::now();
    tick(&mut pc, &mut g, at);
    tick(&mut pc, &mut g, at + Duration::from_secs(5));
    let r = handle(&mut pc, &mut g, Request::Status, 0);
    assert!(
        r.engine.as_ref().is_some_and(|e| e.parked),
        "{:?}",
        r.engine
    );
    assert!(!pc.called(Call::EngineStart));
    assert_eq!((g.state.mode, g.state.job), (Mode::Dev, Some(7)));
}

/// A parked engine outside a HIL job is a fault (#35, supervisor decision of
/// 2026-10-07): its stream stopped with the card held, so nothing plays
/// until the engine ends. The watch alarms it by the engine's state alone (no
/// level, #38), once per parked engine: never inside a HIL job (test #2 parks
/// it on purpose), again only after an engine was seen unparked or the guard
/// started a new one (`spawns`); a look without an engine changes nothing.
/// In every mode: in event an engine of ours runs only after the event plan
/// stopped for the owner (whose own alarm comes besides). It ends nothing.
#[test]
fn a_parked_engine_outside_a_hil_job_alarms_once_until_it_is_no_longer_parked() {
    const PARKED: &str = "the engine's stream is parked outside a HIL job: it holds the card \
                          and nothing plays until the engine ends; nothing is ended";
    let parked = |g: &Guard| texts(g).iter().filter(|t| t.as_str() == PARKED).count();
    let at = Instant::now();
    let s = Duration::from_secs;
    // Inside a job: no alarm, however long it stays parked.
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    g.state.job = Some(7);
    pc.seen.status.parked = true;
    tick(&mut pc, &mut g, at);
    tick(&mut pc, &mut g, at + s(5));
    assert!(g.alarms.all().is_empty(), "{:?}", texts(&g));
    // The job ends while it stays parked: now it is a fault, alarmed once.
    g.state.job = None;
    tick(&mut pc, &mut g, at + s(6));
    tick(&mut pc, &mut g, at + s(7));
    assert_eq!(texts(&g), vec![PARKED.to_owned()]);
    for mode in [Mode::Dev, Mode::Live, Mode::Event] {
        let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(mode));
        // A streaming engine raises nothing.
        tick(&mut pc, &mut g, at);
        assert!(g.alarms.all().is_empty(), "{mode:?}: {:?}", texts(&g));
        // Parked: one alarm, no owner question, its notice sent, however
        // many looks see it.
        pc.seen.status.parked = true;
        for k in 1..4 {
            tick(&mut pc, &mut g, at + s(k));
        }
        assert_eq!(texts(&g), vec![PARKED.to_owned()], "{mode:?}");
        let a = g.alarms.last().unwrap();
        assert!(a.notified && !a.owner_question && a.step.is_none(), "{a:?}");
        assert_eq!(
            pc.notices.last().map(|n| n.2.as_str()),
            Some(PARKED),
            "{mode:?}"
        );
        // No longer parked, then parked again: one more.
        pc.seen.status.parked = false;
        tick(&mut pc, &mut g, at + s(4));
        assert_eq!(parked(&g), 1, "{mode:?}");
        pc.seen.status.parked = true;
        tick(&mut pc, &mut g, at + s(5));
        tick(&mut pc, &mut g, at + s(6));
        assert_eq!(parked(&g), 2, "{mode:?}");
        // A look without an engine (one coming up, or the connection renewed
        // to the same engine) changes nothing: still the one alarm.
        pc.engine_up = false;
        tick(&mut pc, &mut g, at + s(7));
        pc.engine_up = true;
        tick(&mut pc, &mut g, at + s(8));
        assert_eq!(parked(&g), 2, "{mode:?}");
        // A new engine of ours (a respawn or a plan's start, each counted in
        // `spawns`) that parks: one more, even with no look between.
        g.spawns += 1;
        tick(&mut pc, &mut g, at + s(9));
        tick(&mut pc, &mut g, at + s(10));
        assert_eq!(parked(&g), 3, "{mode:?}");
        // The watch ends nothing and starts nothing for it.
        assert!(
            pc.mutating_calls().iter().all(|c| *c == Call::Notify),
            "{mode:?}: {:?}",
            pc.mutating_calls()
        );
    }
}

#[test]
fn an_injected_fault_is_respawned_once_and_reported() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    g.state.job = Some(7);
    let r = handle(&mut pc, &mut g, Request::InjectFault, 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (
            true,
            "the engine faults its RT callback; the watch starts it again"
        )
    );
    assert_eq!(r.engine, Some(engine_of(0, None)));
    // The engine exits 70 (its RT fault); the watch starts it again after
    // the first backoff, with the job's flags, exactly once.
    pc.facts.engine = false;
    pc.exited.push((Kid::Engine, Some(70)));
    let at = Instant::now();
    tick(&mut pc, &mut g, at);
    assert!(!pc.called(Call::EngineStart));
    let r = handle(&mut pc, &mut g, Request::Status, 0);
    assert_eq!(r.engine, None, "no engine between the exit and the respawn");
    tick(&mut pc, &mut g, at + Duration::from_secs(1));
    tick(&mut pc, &mut g, at + Duration::from_secs(5));
    assert_eq!(pc.engine_starts, [(false, true)]);
    let r = handle(&mut pc, &mut g, Request::Status, 0);
    assert_eq!(r.engine, Some(engine_of(1, Some(70))));
    assert_eq!(g.state.mode, Mode::Dev);
    assert!(g.alarms.all().is_empty(), "{:?}", texts(&g));
}

#[test]
fn activate_in_a_job_restarts_the_engine_and_the_server() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    g.state.mode = Mode::Dev;
    g.state.job = Some(7);
    let mut pc = FakePc::new(Facts {
        runner: true,
        ..iemmixer_up()
    });
    let zip = install::tests::good_zip(dir.path(), SHA);
    assert!(install_bundle(&mut g, &zip).0);
    // The pin before this one keeps its Defender exclusions.
    g.state.pins.current = Some(OTHER.into());
    let r = handle(&mut pc, &mut g, Request::Activate { sha: SHA.into() }, 0);
    assert_eq!(pc.excluded, [(SHA.to_owned(), vec![OTHER.to_owned()])]);
    assert_eq!(
        (r.ok, r.detail.clone()),
        (
            true,
            format!(
                "activated {SHA}; the engine and the server run it; the guard hands over to its \
                 new exe; engine started, held, with its HIL flags (pid 1001); engine ready: 32 \
                 frames, 30000 callbacks, 0 missed; server started (pid 1002)"
            )
        )
    );
    assert_eq!(
        steps(&pc)
            .into_iter()
            .filter(|c| c.mutates() || *c == Call::EngineReady)
            .collect::<Vec<_>>(),
        [
            Call::Exclude,
            Call::EngineStop,
            Call::ServerStop,
            Call::PrefCheck,
            Call::EngineStart,
            Call::EngineReady,
            Call::EngineArm,
            Call::ServerStart,
        ]
    );
    assert_eq!(pc.engine_starts, [(true, true)]);
    assert_eq!(g.state.job, Some(7), "the job goes on");
    // The job is in the state: the guard the activation hands over to serves it.
    let back = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    assert_eq!(back.state.job, Some(7));
    // Outside a job the engine is not touched.
    let mut pc = FakePc::new(iemmixer_up());
    g.state.job = None;
    let r = handle(&mut pc, &mut g, Request::Activate { sha: SHA.into() }, 0);
    assert_eq!((r.ok, r.detail.clone()), (true, format!("activated {SHA}")));
    assert!(!pc.called(Call::EngineStop) && !pc.called(Call::EngineStart));
}

/// A guard with bundle `SHA` installed (its files under `dir`), in event,
/// `OTHER` pinned (the running guard's bundle).
fn installed_in_event(dir: &Path) -> Guard {
    let mut g = Guard::open(dir, SiteConf::default(), fixed(T0));
    assert_eq!(g.state.mode, Mode::Event);
    let zip = install::tests::good_zip(dir, SHA);
    assert!(install_bundle(&mut g, &zip).0);
    g.state.pins.current = Some(OTHER.into());
    g
}

/// Every call to REAPER or the predecessor app, reads included.
const BAND_CALLS: [Call; 7] = [
    Call::ReaperSaveQuit,
    Call::AppStop,
    Call::HolderGone,
    Call::ReaperStart,
    Call::ReaperFacts,
    Call::AppStart,
    Call::AppAnswers,
];

/// A guard fix reaches a guard in event (#9 2026-09-28): a guard that
/// refuses the dev entry could never enter dev to take it. In an idle
/// event `activate` copies the bins, pins, sets the exclusions and hands
/// over, with no call to REAPER or the app. The new exe's guard starts as
/// after any guard restart: in event its start runs the event plan's
/// checks, which read REAPER and the app and neither stop nor start them.
#[test]
fn activate_in_an_idle_event_hands_over_and_leaves_reaper_and_the_app_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = installed_in_event(dir.path());
    let mut pc = FakePc::new(band_up());
    let r = handle(&mut pc, &mut g, Request::Activate { sha: SHA.into() }, 0);
    assert_eq!(
        (r.ok, r.detail.clone()),
        (
            true,
            format!("activated {SHA}; the guard hands over to its new exe")
        )
    );
    assert_eq!(pc.calls(), [Call::Facts, Call::SetBundle, Call::Exclude]);
    for c in BAND_CALLS {
        assert!(!pc.called(c), "{c:?}");
    }
    assert_eq!(pc.excluded, [(SHA.to_owned(), vec![OTHER.to_owned()])]);
    assert_eq!(pc.bundle.as_deref(), Some(SHA));
    assert_eq!(
        (g.state.mode, g.state.pins.current.as_deref()),
        (Mode::Event, Some(SHA))
    );
    assert_eq!(
        g.handover,
        Some(install::bin_dir(dir.path()).join(install::GUARD_EXE))
    );
    assert!(g.alarms.all().is_empty(), "{:?}", texts(&g));
    assert_eq!(r.guard_build.as_deref(), Some(GUARD_BUILD));
    // The new exe's guard: the pin it finds, event kept, REAPER and the
    // app neither stopped nor started.
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    let mut pc = FakePc::new(band_up());
    assert_eq!(start(&mut pc, &mut g, 0), Some(Outcome::Done));
    assert_eq!(
        (g.state.mode, g.state.pins.current.as_deref()),
        (Mode::Event, Some(SHA))
    );
    assert_eq!(pc.bundle.as_deref(), Some(SHA));
    for c in [
        Call::ReaperSaveQuit,
        Call::AppStop,
        Call::ReaperStart,
        Call::AppStart,
        Call::EngineStart,
    ] {
        assert!(!pc.called(c), "{c:?}");
    }
}

#[test]
fn activate_in_event_is_refused_while_the_guard_is_not_idle() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = installed_in_event(dir.path());
    let bin_guard = install::bin_dir(dir.path()).join(install::GUARD_EXE);
    let activate = || Request::Activate { sha: SHA.into() };
    // A process of iemmixer's runs.
    let mut pc = FakePc::new(Facts {
        engine: true,
        tray: true,
        ..band_up()
    });
    let r = handle(&mut pc, &mut g, activate(), 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (
            false,
            "activate in event needs no iemmixer process; running: engine, tray"
        )
    );
    assert!(steps(&pc).is_empty(), "{:?}", pc.calls());
    let mut pc = FakePc::new(band_up());
    // A switch persisted as unfinished.
    g.state.switching = Some(Switching {
        from: Mode::Event,
        to: Mode::Dev,
        done: vec![Step::Precheck],
        started: T0,
    });
    let r = handle(&mut pc, &mut g, activate(), 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "a switch is in progress: activate waits for its end")
    );
    g.state.switching = None;
    // A HIL job that did not end.
    g.state.job = Some(7);
    let r = handle(&mut pc, &mut g, activate(), 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "HIL job 7 runs: activate waits for its end")
    );
    // Nothing was copied, pinned, excluded or handed over.
    assert!(steps(&pc).is_empty(), "{:?}", pc.calls());
    assert!(!bin_guard.exists());
    assert_eq!(
        (g.state.pins.current.as_deref(), g.handover.as_ref()),
        (Some(OTHER), None)
    );
    // Idle again: a bundle that is not installed is named.
    g.state.job = None;
    let missing = "f".repeat(40);
    let r = handle(
        &mut pc,
        &mut g,
        Request::Activate {
            sha: missing.clone(),
        },
        0,
    );
    assert_eq!(
        (r.ok, r.detail),
        (false, format!("bundle {missing} is not installed"))
    );
    assert!(steps(&pc).is_empty(), "{:?}", pc.calls());
}

/// Live activates its bundle through `live --build`.
#[test]
fn activate_in_live_is_refused() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Live));
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Green));
    let r = handle(&mut pc, &mut g, Request::Activate { sha: SHA.into() }, 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (
            false,
            "activate is for dev and an idle event; the mode is live (live --build activates its \
             bundle)"
        )
    );
    assert!(steps(&pc).is_empty(), "{:?}", pc.calls());
    assert!(pc.excluded.is_empty());
    assert_eq!(g.state.pins.current, None);
}

/// Every reply names the build of the guard that answered, so a hand-over
/// to a new exe is verifiable (`iempc activate` waits for it).
#[test]
fn every_reply_names_the_guard_s_build() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    let r = ask(&mut pc, &mut g, Request::Status);
    assert_eq!(r.guard_build.as_deref(), Some(GUARD_BUILD));
    let r = ask(&mut pc, &mut g, Request::ProbeTask);
    assert_eq!(r.guard_build.as_deref(), Some(GUARD_BUILD));
    // The pipe answers a status (and a refusal while switching) from the view.
    match g.shared.route(&Request::Status) {
        Route::Now(r) => assert_eq!(r.guard_build.as_deref(), Some(GUARD_BUILD)),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        g.shared
            .view()
            .reply(false, "switching")
            .guard_build
            .as_deref(),
        Some(GUARD_BUILD)
    );
}

/// The offline refusal of a saved mode other than event.
fn offline_mode_refusal(mode: &str) -> String {
    format!("the saved mode is {mode}: without a guard only an idle event activates")
}

/// `iemmixer-guard activate <sha>` while no guard runs (#9 2026-09-28): the
/// way to a guard too old to activate in event (its own code refuses it).
/// Under the guard's mutex, the same rule on the saved state and the
/// process list; then the bins, the pin and the exclusions, saved. It
/// starts no guard: the next `iemmode` call starts the guard's task, which
/// runs the new exe from `bin\`.
#[test]
fn activate_offline_in_an_idle_event_copies_pins_and_starts_no_guard() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = installed_in_event(dir.path());
    let mut pc = FakePc::new(band_up());
    let r = activate_offline(&mut pc, &mut g, Some(()), SHA);
    assert_eq!(
        (r.ok, r.detail.clone()),
        (
            true,
            format!(
                "activated {SHA} without a guard; the next iemmode call starts the guard from bin"
            )
        )
    );
    assert_eq!(pc.calls(), [Call::Facts, Call::SetBundle, Call::Exclude]);
    assert_eq!(pc.excluded, [(SHA.to_owned(), vec![OTHER.to_owned()])]);
    let bin = install::bin_dir(dir.path());
    assert!(bin.join(install::GUARD_EXE).is_file());
    assert!(bin.join(install::IEMMODE_EXE).is_file());
    assert_eq!(g.handover, None, "no guard is started or handed over to");
    assert_eq!(
        (r.mode, r.guard_build.as_deref()),
        (Mode::Event, Some(GUARD_BUILD))
    );
    // Saved: the guard the next iemmode call starts finds the pin.
    let back = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    assert_eq!(
        (back.state.mode, back.state.pins.current.as_deref()),
        (Mode::Event, Some(SHA))
    );
    // Exclusions that fail alarm (the alarm is kept for the next guard);
    // the activation stands.
    let dir = tempfile::tempdir().unwrap();
    let mut g = installed_in_event(dir.path());
    let mut pc = FakePc::new(band_up());
    pc.fail(Call::Exclude, "the exclude task ended with 1");
    let r = activate_offline(&mut pc, &mut g, Some(()), SHA);
    assert!(r.ok, "{r:?}");
    let back = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    assert_eq!(
        texts(&back),
        [format!(
            "Defender exclusions for {SHA}: the exclude task ended with 1"
        )]
    );
    assert_eq!(back.state.pins.current.as_deref(), Some(SHA));
}

#[test]
fn activate_offline_is_refused_while_a_guard_runs_or_the_event_is_not_idle() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = installed_in_event(dir.path());
    let bin_guard = install::bin_dir(dir.path()).join(install::GUARD_EXE);
    // A guard holds the mutex: nothing is read.
    let mut pc = FakePc::new(band_up());
    let r = activate_offline::<()>(&mut pc, &mut g, None, SHA);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "a guard runs; use iemmode activate")
    );
    assert!(pc.calls().is_empty(), "{:?}", pc.calls());
    // Dev and live by the saved mode: a guard serves them.
    for (mode, name) in [(Mode::Dev, "dev"), (Mode::Live, "live")] {
        g.state.mode = mode;
        let r = activate_offline(&mut pc, &mut g, Some(()), SHA);
        assert_eq!((r.ok, r.detail), (false, offline_mode_refusal(name)));
    }
    assert!(pc.calls().is_empty(), "{:?}", pc.calls());
    g.state.mode = Mode::Event;
    // A process of iemmixer's runs.
    let mut pc = FakePc::new(Facts {
        engine: true,
        ..band_up()
    });
    let r = activate_offline(&mut pc, &mut g, Some(()), SHA);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (
            false,
            "activate in event needs no iemmixer process; running: engine"
        )
    );
    let mut pc = FakePc::new(band_up());
    // A bundle that is not installed.
    let r = activate_offline(&mut pc, &mut g, Some(()), OTHER);
    assert_eq!(
        (r.ok, r.detail),
        (false, format!("bundle {OTHER} is not installed"))
    );
    // Nothing was copied, pinned or excluded.
    assert!(steps(&pc).is_empty(), "{:?}", pc.calls());
    assert!(!bin_guard.exists());
    assert_eq!(g.state.pins.current.as_deref(), Some(OTHER));
}

#[test]
fn install_site_in_a_job_restarts_only_the_engine_and_the_server() {
    let (mut pc, mut g) = (
        FakePc::new(Facts {
            runner: true,
            ..iemmixer_up()
        }),
        Guard::for_test(Mode::Dev),
    );
    g.state.pins.current = Some(SHA.into());
    g.state.job = Some(7);
    let site = Request::InstallSite {
        path: "site.toml".into(),
    };
    let r = handle(&mut pc, &mut g, site.clone(), 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (
            true,
            "site installed; the engine and the server run it (HIL job); site.toml: checked and installed; engine started, held, with its HIL flags (pid 1001); engine ready: 32 frames, 30000 callbacks, 0 missed; server started (pid 1002)"
        )
    );
    for c in [
        Call::RunnerStop,
        Call::TrayStop,
        Call::Data,
        Call::Tuning,
        Call::RunnerStart,
    ] {
        assert!(!pc.called(c), "{c:?}: no dev entry inside a job");
    }
    assert_eq!(pc.engine_starts, [(true, true)]);
    assert_eq!(g.state.job, Some(7));
    // The children are saved after the last step too (a guard that takes
    // over adopts the new engine and server).
    assert_eq!(pc.calls_after(Call::ServerStart), [Call::Children]);
    // A failed start unwinds to event, as a failed dev entry does.
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    g.state.pins.current = Some(SHA.into());
    g.state.job = Some(7);
    pc.fail(Call::ServerStart, "ports 80/443 are still held");
    let r = handle(&mut pc, &mut g, site, 0);
    assert!(!r.ok);
    assert!(
        r.detail
            .starts_with("site installed; ServerStart: ports 80/443 are still held; event: done"),
        "{}",
        r.detail
    );
    assert_eq!(g.state.mode, Mode::Event);
    assert!(pc.called(Call::ReaperStart));
    assert_eq!(texts(&g)[0], "ServerStart: ports 80/443 are still held");
    assert_eq!(g.state.job, None, "no HIL job outside dev");
}

/// Inside a HIL job the engine starts without a plan (`restart_in_job`,
/// for activate and install-site): an engine that ended holding the card
/// left 32, and the new one refuses the card unless it finds REAPER's
/// original (exit 3, #9 2026-09-28). So the same check runs right before
/// the start, after the old engine stopped. A failed restore starts
/// nothing, alarms and unwinds to event, as any failed step there.
#[test]
fn a_job_restart_restores_the_preference_right_before_the_engine_starts() {
    let (mut pc, mut g) = (
        FakePc::new(Facts {
            runner: true,
            ..iemmixer_up()
        }),
        Guard::for_test(Mode::Dev),
    );
    g.state.pins.current = Some(SHA.into());
    g.state.job = Some(7);
    pc.pref_attempts = 1;
    let site = Request::InstallSite {
        path: "site.toml".into(),
    };
    let r = handle(&mut pc, &mut g, site.clone(), 0);
    assert!(r.ok, "{r:?}");
    assert!(
        r.detail
            .contains("; the preferred buffer was restored (1 writes); engine started"),
        "{}",
        r.detail
    );
    assert_eq!(
        steps(&pc)
            .into_iter()
            .filter(|c| c.mutates())
            .collect::<Vec<_>>(),
        [
            Call::InstallSite,
            Call::EngineStop,
            Call::ServerStop,
            Call::PrefCheck,
            Call::EngineStart,
            Call::EngineArm,
            Call::ServerStart,
        ]
    );
    assert_eq!(pc.pref_writes, 1);
    assert_eq!(pc.engine_starts, [(true, true)]);
    assert_eq!(g.state.job, Some(7));

    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    g.state.pins.current = Some(SHA.into());
    g.state.job = Some(7);
    pc.fail(
        Call::PrefCheck,
        "writing the preferred buffer failed: access denied",
    );
    let r = handle(&mut pc, &mut g, site, 0);
    assert!(!r.ok);
    assert!(
        r.detail.starts_with(
            "site installed; PrefCheck: writing the preferred buffer failed: access denied; \
             event: done"
        ),
        "{}",
        r.detail
    );
    assert!(!pc.called(Call::EngineStart));
    assert_eq!(
        texts(&g)[0],
        "PrefCheck: writing the preferred buffer failed: access denied"
    );
    assert_eq!(g.state.mode, Mode::Event);
    assert!(pc.called(Call::ReaperStart));
    assert_eq!(g.state.job, None);
}

#[test]
fn install_site_checks_the_site_and_enters_dev_again() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    g.state.pins.current = Some(SHA.into());
    pc.fail(Call::InstallSite, "check-site ended with Some(2)");
    let r = handle(
        &mut pc,
        &mut g,
        Request::InstallSite {
            path: "bad.toml".into(),
        },
        0,
    );
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "site refused: check-site ended with Some(2)")
    );
    assert!(!pc.called(Call::EngineStop));
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    g.state.pins.current = Some(SHA.into());
    let r = handle(
        &mut pc,
        &mut g,
        Request::InstallSite {
            path: "site.toml".into(),
        },
        0,
    );
    assert!(r.ok, "{r:?}");
    assert!(
        r.detail
            .starts_with("site installed; dev: done; site.toml: checked and installed"),
        "{}",
        r.detail
    );
    assert!(pc.index(Call::InstallSite) < pc.index(Call::EngineStop));
    assert!(pc.index(Call::EngineStop) < pc.index(Call::EngineStart));
    assert!(pc.called(Call::ServerStart));
    assert!(!pc.called(Call::ReaperStart));
    assert_eq!(g.state.mode, Mode::Dev);
}

#[test]
fn an_install_site_whose_dev_entry_unwinds_is_no_success() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    g.state.pins.current = Some(SHA.into());
    pc.fail(Call::ServerStart, "ports 80/443 are still held");
    let r = handle(
        &mut pc,
        &mut g,
        Request::InstallSite {
            path: "site.toml".into(),
        },
        0,
    );
    assert!(!r.ok);
    assert!(
        r.detail
            .starts_with("site installed; dev: not entered; unwound to event"),
        "{}",
        r.detail
    );
    assert_eq!(pc.sites, ["site.toml"]);
    assert_eq!(g.state.mode, Mode::Event);
    assert!(pc.index(Call::ReaperStart) > pc.index(Call::ServerStart));
}

#[test]
fn ide_event_ends_the_site_check_at_once() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    g.state.pins.current = Some(SHA.into());
    pc.block_until_cancel(Call::InstallSite);
    let shared = Arc::clone(&g.shared);
    let fired = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        // No switch runs: "ide event" pre-empts the token and queues.
        let route = shared.route(&Request::Event { dry_run: false });
        (route, Instant::now())
    });
    let r = handle(
        &mut pc,
        &mut g,
        Request::InstallSite {
            path: "site.toml".into(),
        },
        0,
    );
    let (route, at) = fired.join().unwrap();
    assert_eq!(route, Route::Queue(0));
    assert!(at.elapsed() < Duration::from_secs(1), "{:?}", at.elapsed());
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "site refused: pre-empted by event")
    );
    assert!(pc.sites.is_empty());
    assert!(!pc.called(Call::EngineStop));
}

#[test]
fn rehearse_teardown_never_starts_reaper() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    g.state.pins.current = Some(SHA.into());
    let r = handle(&mut pc, &mut g, Request::RehearseTeardown, 0);
    assert!(r.ok, "{r:?}");
    assert!(
        r.detail.starts_with(
            "teardown clean: module unheld, preference original, ports 80/443 free; dev: done"
        ),
        "{}",
        r.detail
    );
    for c in [
        Call::ReaperStart,
        Call::ReaperSaveQuit,
        Call::ReaperFacts,
        Call::AppStart,
        Call::AppStop,
        Call::AppAnswers,
    ] {
        assert!(!pc.called(c), "{c:?}");
    }
    let order = steps(&pc);
    let first = |c: Call| order.iter().position(|x| *x == c).unwrap();
    assert!(first(Call::EngineStop) < first(Call::ServerStop));
    assert!(first(Call::ServerStop) < first(Call::TrayStop));
    assert!(first(Call::TrayStop) < first(Call::Tuning));
    assert!(first(Call::Tuning) < first(Call::PrefCheck));
    // The teardown's, the rehearsal's own check, and the dev re-entry's
    // right before the engine starts.
    assert_eq!(pc.count(Call::PrefCheck), 3);
    assert!(pc.index(Call::EngineStart) > pc.index(Call::EngineStop));
    assert_eq!(g.state.mode, Mode::Dev);
    assert!(g.alarms.all().is_empty());
    // Not a switch: only the re-entry into dev counts.
    assert_eq!(g.shared.view().epoch, 1);
}

#[test]
fn a_rehearsal_that_finds_problems_says_so() {
    // The preference needed a write: reported, alarmed, dev entered again.
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    g.state.pins.current = Some(SHA.into());
    pc.pref_attempts = 1;
    let r = ask(&mut pc, &mut g, Request::RehearseTeardown);
    assert!(!r.ok);
    assert!(
        r.detail
            .starts_with("teardown problems: the preference needed 1 writes; dev: done"),
        "{}",
        r.detail
    );
    assert_eq!(
        texts(&g),
        ["rehearsal: teardown problems: the preference needed 1 writes"]
    );
    assert!(pc.called(Call::EngineStart));
    // Ports 80/443 still held after the teardown: the predecessor could not
    // serve the band (#9 2026-09-28); an unreadable owner is named too.
    for (ports, named) in [
        (
            Ok((Some(4242), None)),
            "ports 80/443 are still held (80: 4242, 443: free)",
        ),
        (
            Ok((None, Some(77))),
            "ports 80/443 are still held (80: free, 443: 77)",
        ),
        (Err("no table".to_owned()), "ports 80/443: no table"),
    ] {
        let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
        g.state.pins.current = Some(SHA.into());
        pc.ports = ports;
        let r = ask(&mut pc, &mut g, Request::RehearseTeardown);
        assert!(!r.ok);
        assert!(
            r.detail
                .starts_with(&format!("teardown problems: {named}; dev: done")),
            "{}",
            r.detail
        );
        assert_eq!(
            texts(&g),
            [format!("rehearsal: teardown problems: {named}")]
        );
    }
    // A holder that stays and processes that stay are named; the re-entry
    // that then fails stops and asks the owner, never starting REAPER.
    let (mut pc, mut g) = (
        FakePc::new(Facts {
            other_module_holder: true,
            ..iemmixer_up()
        }),
        Guard::for_test(Mode::Dev),
    );
    g.state.pins.current = Some(SHA.into());
    pc.fail(Call::TrayStop, "the tray did not quit within 10 s");
    pc.fail(Call::PrefCheck, "the registry is locked");
    let r = ask(&mut pc, &mut g, Request::RehearseTeardown);
    assert!(!r.ok);
    assert!(
        r.detail.starts_with(
            "teardown problems: iemmixer processes still run; the driver module is held; \
             the preference: the registry is locked; dev: stopped; the owner decides; \
             the mode is dev"
        ),
        "{}",
        r.detail
    );
    assert_eq!(
        texts(&g)[0],
        "TrayStop: rehearsal: the tray did not quit within 10 s"
    );
    let last = g.alarms.last().unwrap();
    assert_eq!(
        last.text,
        "TrayStop: the tray did not quit within 10 s; the rehearsal never starts REAPER"
    );
    assert!(last.owner_question);
    assert!(!pc.called(Call::ReaperStart) && !pc.called(Call::AppStart));
    assert_eq!(g.state.mode, Mode::Dev);
    // The flag ends with the rehearsal: a failed dev entry unwinds again.
    let r = ask(&mut pc, &mut g, dev());
    assert!(!r.ok);
    assert!(pc.called(Call::ReaperStart));
    // A healthy engine that does not release stops the rehearsal.
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    pc.fail(Call::EngineStop, "no DriverReleased within 10 s");
    pc.health(Health::Healthy);
    let r = ask(&mut pc, &mut g, Request::RehearseTeardown);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (
            false,
            "rehearsal stopped at EngineStop: no DriverReleased within 10 s"
        )
    );
    assert!(!pc.called(Call::ServerStop) && !pc.called(Call::EngineStart));
    let a = g.alarms.last().unwrap();
    assert!(a.owner_question);
    assert_eq!(
        a.text,
        "EngineStop: rehearsal: no DriverReleased within 10 s; engine healthy, iemmixer keeps serving"
    );
    assert_eq!(g.state.mode, Mode::Dev);
    // A dead one too, asking the owner.
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    pc.fail(Call::EngineStop, "refused");
    let r = ask(&mut pc, &mut g, Request::RehearseTeardown);
    assert!(!r.ok);
    assert_eq!(
        g.alarms.last().unwrap().text,
        "EngineStop: rehearsal: refused; health Some(Dead)"
    );
    assert!(!pc.called(Call::ServerStop));
}

#[test]
fn alarm_test_ack_status_quit_and_subscribe() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    let r = handle(&mut pc, &mut g, Request::AlarmTest, 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (true, "the test alarm reached the engineer's devices")
    );
    assert_eq!(
        pc.notices,
        [(
            Audience::Alarm,
            "iemmixer alarm".to_owned(),
            "alarm test (iemmode alarm-test)".to_owned()
        )]
    );
    assert_eq!(r.alarms.len(), 1);
    let id = r.alarms[0].id;
    let r = handle(&mut pc, &mut g, Request::AlarmAck { id }, 0);
    assert_eq!(
        (r.ok, r.detail.clone()),
        (true, format!("alarm {id} acknowledged"))
    );
    assert!(r.alarms[0].acked);
    let r = handle(&mut pc, &mut g, Request::AlarmAck { id: 99 }, 0);
    assert_eq!((r.ok, r.detail.as_str()), (false, "no alarm 99"));
    pc.fail(Call::Notify, "no device took the notice");
    let r = handle(&mut pc, &mut g, Request::AlarmTest, 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "the test alarm was not delivered")
    );
    let r = handle(&mut pc, &mut g, Request::Status, 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (true, "mode event; no bundle; 1 unacknowledged alarms")
    );
    let r = handle(&mut pc, &mut g, Request::Subscribe, 0);
    assert!(r.ok);
    assert!(!g.quit);
    let r = handle(&mut pc, &mut g, Request::Quit, 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (true, "the guard stops; its children keep running")
    );
    assert!(g.quit);
}

#[test]
fn status_names_everything_that_waits() {
    let mut g = Guard::for_test(Mode::Dev);
    assert_eq!(status_text(&g), "mode dev; no bundle");
    g.state.pins.current = Some(SHA.into());
    g.state.job = Some(4242);
    g.raise(None, "one", false);
    g.raise(None, "two", false);
    assert_eq!(
        status_text(&g),
        format!("mode dev; bundle {SHA}; HIL job 4242; 2 unacknowledged alarms")
    );
    assert_eq!(g.shared.view().status, status_text(&g));
    assert_eq!(
        [Mode::Event, Mode::Dev, Mode::Live].map(mode_name),
        ["event", "dev", "live"]
    );
}

/// On the PC the guard's task job allows no breakaway (#9 2026-09-28): a
/// guard whose children stay in its job (one that does not end its
/// processes when it closes) names it in `iemmode status` and the tray's
/// view from its start on, for its whole life; any other reading of the
/// job names nothing (a refusal is each start's step error).
#[test]
fn a_guard_whose_children_stay_in_its_job_names_it() {
    const NOTE: &str = "children stay in the guard task's job (no breakaway)";
    let mut g = Guard::for_test(Mode::Dev);
    let mut pc = FakePc::new(iemmixer_up());
    pc.job = Ok(Placement::InJob);
    assert_eq!(start(&mut pc, &mut g, 0), None);
    assert_eq!(status_text(&g), format!("mode dev; no bundle; {NOTE}"));
    assert_eq!(g.shared.view().status, status_text(&g));
    g.raise(None, "one", false);
    assert_eq!(
        status_text(&g),
        format!("mode dev; no bundle; {NOTE}; 1 unacknowledged alarms")
    );
    for job in [
        Ok(Placement::Breakaway),
        Ok(Placement::NoJob),
        Ok(Placement::Refuse("the job ends its processes")),
        Err("the job could not be read".to_owned()),
    ] {
        let mut g = Guard::for_test(Mode::Dev);
        let mut pc = FakePc::new(iemmixer_up());
        pc.job = job.clone();
        assert_eq!(start(&mut pc, &mut g, 0), None, "{job:?}");
        assert_eq!(status_text(&g), "mode dev; no bundle", "{job:?}");
        assert_eq!(g.shared.view().status, status_text(&g), "{job:?}");
    }
}

#[test]
fn texts_are_cut_to_fit_one_frame() {
    assert_eq!(cut("abcdé", 4), "abcd");
    assert_eq!(cut("čšž", 3), "čšž");
    assert_eq!(cut("čšž", 2), "čš");
    let mut g = Guard::for_test(Mode::Event);
    g.raise(None, &"x".repeat(ALARM_CHARS + 10), false);
    assert_eq!(g.alarms.last().unwrap().text.len(), ALARM_CHARS);
    assert_eq!((ALARM_CHARS, DETAIL_CHARS), (600, 8000));
    let long = "y".repeat(DETAIL_CHARS + 1);
    assert_eq!(g.reply(true, &long).detail.len(), DETAIL_CHARS);
    assert_eq!(
        g.shared.view().reply(true, &long).detail.len(),
        DETAIL_CHARS
    );
}

#[test]
fn notices_go_to_the_engineers_devices_once() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    g.raise(None, "x", false);
    send_notices(&mut pc, &mut g);
    assert_eq!(
        pc.notices,
        [(Audience::Alarm, "iemmixer alarm".to_owned(), "x".to_owned())]
    );
    assert!(g.alarms.last().unwrap().notified);
    assert!(g.shared.view().alarms[0].notified);
    send_notices(&mut pc, &mut g);
    assert_eq!(pc.count(Call::Notify), 1);
    // A failed notice is tried once, not at every look.
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    pc.fail(Call::Notify, "no bundle is active");
    g.raise(None, "y", false);
    send_notices(&mut pc, &mut g);
    send_notices(&mut pc, &mut g);
    assert_eq!(pc.count(Call::Notify), 1);
    assert!(!g.alarms.last().unwrap().notified);
    // Alarms raised meanwhile are sent in order.
    g.raise(None, "z1", false);
    g.raise(None, "z2", true);
    let mut ok = FakePc::new(Facts::default());
    send_notices(&mut ok, &mut g);
    let sent: Vec<&str> = ok.notices.iter().map(|n| n.2.as_str()).collect();
    assert_eq!(sent, ["z1", "z2"]);
    let notified = g.shared.view().alarms.iter().filter(|a| a.notified).count();
    assert_eq!(notified, 2);
}

/// A guard that starts again (a hand-over, a restart) reads the alarm file
/// and tries the alarms that never reached a device: only the open ones. An
/// acknowledged alarm never goes to a phone (#9 2026-09-28: a new guard sent
/// six acknowledged alarms from the morning once a device existed).
#[test]
fn an_acknowledged_alarm_is_never_sent() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Dev));
    g.raise(None, "old, acknowledged", false);
    let old = g.alarms.last().unwrap().id;
    assert!(g.alarms.ack(old));
    g.raise(None, "old, open", false);
    send_notices(&mut pc, &mut g);
    let sent: Vec<&str> = pc.notices.iter().map(|n| n.2.as_str()).collect();
    assert_eq!(sent, ["old, open"]);
    assert!(!g.alarms.iter().find(|a| a.id == old).unwrap().notified);
}

// ---- the watch ----

#[test]
fn session_end_stops_respawning() {
    let (mut pc, mut g) = (
        FakePc::new(Facts {
            server: true,
            tray: true,
            ..Facts::default()
        }),
        Guard::for_test(Mode::Dev),
    );
    g.session_ending.store(true, Ordering::SeqCst);
    pc.exited.push((Kid::Engine, Some(70)));
    let at = Instant::now();
    tick(&mut pc, &mut g, at);
    assert!(pc.called(Call::ServerStop) && pc.called(Call::TrayStop));
    assert!(g.shared.await_session_done(Duration::ZERO));
    tick(&mut pc, &mut g, at + Duration::from_secs(20));
    assert!(!pc.called(Call::EngineStart));
    assert_eq!(pc.count(Call::ServerStop), 1);
    assert!(g.alarms.all().is_empty());
    // The engine gets its time to release the card by itself.
    let (mut pc, mut g) = (
        FakePc::new(Facts {
            engine: true,
            ..Facts::default()
        }),
        Guard::for_test(Mode::Dev),
    );
    g.session_wait = Duration::from_millis(300);
    let t = Instant::now();
    session_end(&mut pc, &mut g);
    assert!(t.elapsed() >= Duration::from_millis(300));
    assert!(t.elapsed() < Duration::from_secs(3));
    assert!(pc.count(Call::Procs) >= 2);
    assert!(!pc.called(Call::ServerStop) && !pc.called(Call::TrayStop));
    assert!(!pc.called(Call::EngineStop));
    assert_eq!(SESSION_ENGINE_WAIT, Duration::from_secs(10));
    // Without a session end the waiter gives up at its limit.
    let shared = Arc::new(Shared::new(Cancel::default()));
    let t = Instant::now();
    assert!(!shared.await_session_done(Duration::from_millis(50)));
    assert!(t.elapsed() >= Duration::from_millis(50));
    // The session window waits for the guard's end of the session.
    let other = Arc::clone(&shared);
    let done = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        other.update(|v| v.session_done = true);
    });
    let t = Instant::now();
    assert!(shared.await_session_done(Duration::from_secs(5)));
    assert!(t.elapsed() >= Duration::from_millis(150));
    done.join().unwrap();
}

#[test]
fn session_end_does_not_wait_when_no_engine_is_left() {
    // A bounded, deterministic catch for both loop-condition mutants of
    // `while !p.engine.is_empty() && start.elapsed() < g.session_wait`
    // (daemon.rs:2149). With no engine process left the real loop never runs
    // and `session_end` returns at once. The `&&`->`||` mutant
    // (`!p.engine.is_empty() || elapsed < wait`) and the `delete !` mutant
    // (`p.engine.is_empty() && elapsed < wait`) both keep looping until the full
    // session_wait even with an already-empty engine list. Give session_wait a
    // long value and bound the call on a thread: the original returns in
    // microseconds; either mutant waits ~60 s, so this fails its assertion in
    // 3 s (a clean FAIL) instead of hanging until nextest's slow-timeout (#23,
    // and it runs first under `priority = 100`).
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        // Facts::default() -> the engine process list is empty.
        let mut pc = FakePc::new(Facts::default());
        let mut g = Guard::for_test(Mode::Dev);
        g.session_wait = Duration::from_secs(60);
        session_end(&mut pc, &mut g);
        let _ = done_tx.send(());
    });
    assert!(
        done_rx.recv_timeout(Duration::from_secs(3)).is_ok(),
        "session_end waited on the engine although its process list was empty"
    );
}

#[test]
fn an_engine_exit_respawns_after_the_backoff() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Dev));
    pc.exited.push((Kid::Engine, Some(70)));
    let at = Instant::now();
    tick(&mut pc, &mut g, at);
    assert!(!pc.called(Call::EngineStart));
    tick(&mut pc, &mut g, at + Duration::from_millis(999));
    assert!(!pc.called(Call::EngineStart));
    tick(&mut pc, &mut g, at + Duration::from_secs(1));
    assert_eq!(pc.count(Call::EngineStart), 1);
    tick(&mut pc, &mut g, at + Duration::from_secs(5));
    assert_eq!(pc.count(Call::EngineStart), 1);
    // A failed respawn alarms.
    pc.exited.push((Kid::Engine, None));
    pc.fail(Call::EngineStart, "no bundle is active");
    let later = at + Duration::from_secs(10);
    tick(&mut pc, &mut g, later);
    tick(&mut pc, &mut g, later + Duration::from_secs(2));
    assert_eq!(
        texts(&g),
        ["the engine could not be started again: no bundle is active"]
    );
    // In event no engine is started again.
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    pc.exited.push((Kid::Engine, Some(70)));
    tick(&mut pc, &mut g, at);
    tick(&mut pc, &mut g, at + Duration::from_secs(5));
    assert!(!pc.called(Call::EngineStart));
}

/// An engine that ended while it held the card (a hard kill, a crash) left
/// 32, and a new engine refuses the card unless it finds REAPER's original
/// (exit 3, #9 2026-09-28). The crash watch's respawn starts an engine
/// without a plan, so it runs the same check right before the start (the
/// old engine is gone: nothing holds the driver). A failed restore starts
/// nothing and alarms, as a failed start does; a held driver is never
/// written under.
#[test]
fn a_respawn_restores_the_preference_right_before_the_engine_starts() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Dev));
    pc.pref_attempts = 1;
    pc.exited.push((Kid::Engine, Some(70)));
    let at = Instant::now();
    tick(&mut pc, &mut g, at);
    assert!(!pc.called(Call::PrefCheck));
    tick(&mut pc, &mut g, at + Duration::from_secs(1));
    assert_eq!(
        pc.calls_after(Call::Procs),
        [Call::PrefCheck, Call::EngineStart, Call::Children]
    );
    assert_eq!(pc.pref_writes, 1);
    assert_eq!(pc.engine_starts, [(false, false)]);
    assert!(g.alarms.all().is_empty(), "{:?}", texts(&g));

    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Dev));
    pc.fail(
        Call::PrefCheck,
        "writing the preferred buffer failed: access denied",
    );
    pc.exited.push((Kid::Engine, Some(70)));
    tick(&mut pc, &mut g, at);
    tick(&mut pc, &mut g, at + Duration::from_secs(1));
    tick(&mut pc, &mut g, at + Duration::from_secs(20));
    assert_eq!(pc.count(Call::PrefCheck), 1);
    assert!(!pc.called(Call::EngineStart));
    assert_eq!(
        texts(&g),
        [
            "the engine could not be started again: writing the preferred buffer failed: access \
          denied"
        ]
    );
    assert_eq!(g.state.mode, Mode::Dev);

    // REAPER started meanwhile and holds the card at 32: nothing written,
    // no engine, an alarm names it.
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Dev));
    pc.pref_attempts = 1;
    pc.exited.push((Kid::Engine, Some(70)));
    tick(&mut pc, &mut g, at);
    tick(&mut pc, &mut g, at + Duration::from_secs(1));
    assert!(pc.called(Call::PrefCheck));
    assert!(!pc.called(Call::EngineStart));
    assert_eq!(pc.pref_writes, 0);
    let want = format!("the engine could not be started again: {HELD}");
    assert!(texts(&g).contains(&want), "{:?}", texts(&g));
}

#[test]
fn a_respawn_due_after_ide_event_starts_nothing() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    pc.facts.engine = false;
    pc.exited.push((Kid::Engine, Some(70)));
    let at = Instant::now();
    tick(&mut pc, &mut g, at);
    let r = handle(&mut pc, &mut g, Request::Event { dry_run: false }, 0);
    assert!(r.ok, "{r:?}");
    tick(&mut pc, &mut g, at + Duration::from_secs(5));
    assert!(!pc.called(Call::EngineStart));
}

#[test]
fn clean_and_hopeless_engine_exits_stay_down() {
    for (code, alarm) in [
        (Some(0), None),
        (Some(2), Some("engine site or usage error (exit Some(2))")),
        (Some(3), Some("the card refused the engine (exit Some(3))")),
    ] {
        let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Dev));
        pc.exited.push((Kid::Engine, code));
        let at = Instant::now();
        tick(&mut pc, &mut g, at);
        tick(&mut pc, &mut g, at + Duration::from_secs(20));
        assert!(!pc.called(Call::EngineStart), "{code:?}");
        let want: Vec<String> = alarm.map(str::to_owned).into_iter().collect();
        assert_eq!(texts(&g), want, "{code:?}");
    }
}

#[test]
fn clean_and_session_end_exits_are_no_crashes() {
    // A clean exit, then two crashes within 10 min: no crash loop, and the
    // respawn after the second crash waits 2 s (not the third's 4 s).
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Dev));
    pc.exited = vec![
        (Kid::Engine, Some(0)),
        (Kid::Engine, Some(70)),
        (Kid::Engine, Some(70)),
    ];
    let at = Instant::now();
    tick(&mut pc, &mut g, at);
    assert_eq!(g.state.mode, Mode::Dev);
    assert!(!pc.called(Call::ReaperStart));
    assert!(g.alarms.all().is_empty(), "{:?}", texts(&g));
    tick(&mut pc, &mut g, at + Duration::from_millis(1999));
    assert!(!pc.called(Call::EngineStart));
    tick(&mut pc, &mut g, at + Duration::from_secs(2));
    assert_eq!(pc.count(Call::EngineStart), 1);
    // An exit while the session ends counts neither: two crashes after it
    // make no loop.
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Dev));
    g.session_ending.store(true, Ordering::SeqCst);
    pc.exited = vec![(Kid::Engine, Some(70))];
    tick(&mut pc, &mut g, at);
    g.session_ending.store(false, Ordering::SeqCst);
    pc.exited = vec![(Kid::Engine, Some(70)), (Kid::Engine, Some(70))];
    tick(&mut pc, &mut g, at + Duration::from_secs(1));
    assert_eq!(g.state.mode, Mode::Dev);
    assert!(!pc.called(Call::ReaperStart));
    assert!(g.alarms.all().is_empty(), "{:?}", texts(&g));
}

#[test]
fn a_busy_state_directory_is_tried_again_without_a_crash_count() {
    // #32 minor-4: exit 75 = the engine waited 3 s for its state directory
    // (an engine that just ended may hold its lock a moment). No crash:
    // three in a row make no loop, the guard starts it again after 2 s and
    // alarms once, at the third in a row.
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Dev));
    pc.exited = vec![(Kid::Engine, Some(75)); 3];
    let at = Instant::now();
    tick(&mut pc, &mut g, at);
    assert_eq!(g.state.mode, Mode::Dev);
    assert!(!pc.called(Call::ReaperStart));
    assert_eq!(
        texts(&g),
        [
            "the engine's state directory stayed in use 3 times in a row (exit 75): starting it again"
        ]
    );
    tick(&mut pc, &mut g, at + Duration::from_millis(1999));
    assert!(!pc.called(Call::EngineStart));
    tick(&mut pc, &mut g, at + Duration::from_secs(2));
    assert_eq!(pc.count(Call::EngineStart), 1);
    // They counted no crash: two crashes after them make no loop either,
    // and a fourth busy exit after a crash starts a new streak (no alarm).
    pc.exited = vec![
        (Kid::Engine, Some(70)),
        (Kid::Engine, Some(75)),
        (Kid::Engine, Some(70)),
    ];
    tick(&mut pc, &mut g, at + Duration::from_secs(3));
    assert_eq!(g.state.mode, Mode::Dev);
    assert!(!pc.called(Call::ReaperStart));
    assert_eq!(g.alarms.all().len(), 1, "{:?}", texts(&g));
}

#[test]
fn a_state_directory_that_stays_busy_falls_back_like_a_crash_loop() {
    // F3 round 4, finding 3: exit 75 was tried again without an end, so a
    // state directory held by something the guard does not watch kept the
    // crash loop's fallback (REAPER, or the previous pin in prod) from
    // ever running. Ten busy exits in a row (about 50 s) are still no
    // crash; each one after them counts as abnormal. The alarm at the
    // third stays the only busy alarm.
    let busy = "the engine's state directory stayed in use 3 times in a row (exit 75): \
                starting it again";
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Dev));
    pc.exited = vec![(Kid::Engine, Some(75)); 10];
    let at = Instant::now();
    tick(&mut pc, &mut g, at);
    assert_eq!(g.state.mode, Mode::Dev);
    assert_eq!(texts(&g), [busy]);
    tick(&mut pc, &mut g, at + Duration::from_millis(1999));
    assert!(!pc.called(Call::EngineStart));
    tick(&mut pc, &mut g, at + Duration::from_secs(2));
    assert_eq!(pc.count(Call::EngineStart), 1);
    // The eleventh in a row is a crash: the backoff's 1 s, not 2 s.
    pc.exited = vec![(Kid::Engine, Some(75))];
    let later = at + Duration::from_secs(10);
    tick(&mut pc, &mut g, later);
    tick(&mut pc, &mut g, later + Duration::from_secs(1));
    assert_eq!(pc.count(Call::EngineStart), 2);
    assert_eq!(g.state.mode, Mode::Dev);
    // Two more make three crashes in 10 min: back to REAPER.
    pc.exited = vec![(Kid::Engine, Some(75)); 2];
    tick(&mut pc, &mut g, later + Duration::from_secs(5));
    assert_eq!(g.state.mode, Mode::Event);
    let t = texts(&g);
    assert_eq!(t.iter().filter(|a| *a == busy).count(), 1, "{t:?}");
    let crashed = "the engine crashed 3 times in 10 min: back to REAPER";
    assert!(t.iter().any(|a| a == crashed), "{t:?}");
    assert!(pc.called(Call::ReaperStart));
}

#[test]
fn a_crash_loop_goes_back_to_reaper() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Dev));
    pc.exited = vec![(Kid::Engine, Some(70)); 3];
    let at = Instant::now();
    tick(&mut pc, &mut g, at);
    assert_eq!(g.state.mode, Mode::Event);
    assert_eq!(
        texts(&g),
        ["the engine crashed 3 times in 10 min: back to REAPER"]
    );
    assert!(pc.called(Call::ReaperStart) && pc.called(Call::AppStart));
    tick(&mut pc, &mut g, at + Duration::from_secs(20));
    assert!(!pc.called(Call::EngineStart));
}

#[test]
fn a_crash_loop_in_prod_falls_back_to_the_previous_pin() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Live));
    g.site.prod = true;
    g.state.pins = Pins {
        current: Some(SHA.into()),
        previous: Some(OTHER.into()),
    };
    pc.exited = vec![(Kid::Engine, Some(70)); 3];
    tick(&mut pc, &mut g, Instant::now());
    assert_eq!(g.state.mode, Mode::Live);
    assert_eq!(g.state.pins.current.as_deref(), Some(OTHER));
    assert_eq!(pc.bundle.as_deref(), Some(OTHER));
    assert_eq!(pc.count(Call::EngineStart), 1);
    assert_eq!(
        texts(&g),
        [format!(
            "the engine crashed 3 times in 10 min: back to the previous pin {OTHER}"
        )]
    );
    // Without a previous pin it alarms and stays.
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Live));
    g.site.prod = true;
    pc.exited = vec![(Kid::Engine, Some(70)); 3];
    tick(&mut pc, &mut g, Instant::now());
    assert_eq!(
        texts(&g),
        ["crash loop in prod: no previous pin to revert to"]
    );
    assert!(!pc.called(Call::EngineStart));
}

#[test]
fn other_children_ending() {
    for (mode, kid, alarm) in [
        (Mode::Dev, Kid::Server, true),
        (Mode::Live, Kid::Server, true),
        (Mode::Event, Kid::Server, false),
        (Mode::Dev, Kid::Tray, false),
        (Mode::Dev, Kid::Runner, false),
    ] {
        let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(mode));
        pc.exited.push((kid, Some(1)));
        tick(&mut pc, &mut g, Instant::now());
        let want: Vec<String> = if alarm {
            vec!["the server ended (Some(1))".to_owned()]
        } else {
            Vec::new()
        };
        assert_eq!(texts(&g), want, "{mode:?} {kid:?}");
    }
}

#[test]
fn reaper_or_the_app_appearing_in_dev_alarms_once_per_appearance() {
    let (mut pc, mut g) = (
        FakePc::new(Facts {
            reaper: true,
            ..Facts::default()
        }),
        Guard::for_test(Mode::Dev),
    );
    let at = Instant::now();
    tick(&mut pc, &mut g, at);
    tick(&mut pc, &mut g, at);
    assert_eq!(g.alarms.all().len(), 1);
    assert_eq!(
        g.alarms.all()[0].text,
        "REAPER or the predecessor app started while iemmixer runs; nothing is ended"
    );
    pc.facts.reaper = false;
    tick(&mut pc, &mut g, at);
    pc.facts.app = true;
    tick(&mut pc, &mut g, at);
    assert_eq!(g.alarms.all().len(), 2);
    assert!(pc.mutating_calls().iter().all(|c| *c == Call::Notify));
    // In event the band's system is simply up.
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    tick(&mut pc, &mut g, at);
    assert!(g.alarms.all().is_empty());
}

#[test]
fn drift_is_read_hourly_and_after_each_switch() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    let at = Instant::now();
    tick(&mut pc, &mut g, at);
    assert_eq!(pc.count(Call::TuningDrift), 1);
    tick(&mut pc, &mut g, at + DRIFT_EVERY - Duration::from_millis(1));
    assert_eq!(pc.count(Call::TuningDrift), 1);
    pc.drift = Some("power plan changed".into());
    tick(&mut pc, &mut g, at + DRIFT_EVERY);
    assert_eq!(pc.count(Call::TuningDrift), 2);
    assert_eq!(texts(&g), ["tuning drift: power plan changed"]);
    assert_eq!(DRIFT_EVERY, Duration::from_secs(3600));
    // A switch reads it again.
    run_switch(&mut pc, &mut g, Mode::Event, Mode::Event);
    assert_eq!(pc.count(Call::TuningDrift), 3);
    // An unreadable drift is only logged.
    pc.fail(Call::TuningDrift, "no record");
    run_switch(&mut pc, &mut g, Mode::Event, Mode::Event);
    assert_eq!(g.alarms.all().len(), 2);
}

#[test]
fn drift_is_read_after_every_mode_change() {
    // A dev → event plan that stopped for the owner changed the mode: the
    // watch reads the drift at its next tick (the stopped plan itself makes
    // no call after the health read).
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    let at = Instant::now();
    tick(&mut pc, &mut g, at);
    assert_eq!(pc.count(Call::TuningDrift), 1);
    pc.fail(Call::EngineStop, "no DriverReleased within 10 s");
    pc.health(Health::Parked);
    assert_eq!(
        run_switch(&mut pc, &mut g, Mode::Dev, Mode::Event),
        Outcome::NeedsOwner
    );
    assert_eq!(pc.calls_after(Call::EngineHealth), Vec::<Call>::new());
    tick(&mut pc, &mut g, at + TICK);
    assert_eq!(pc.count(Call::TuningDrift), 2);
    // A kept-serving engine changed nothing: the drift waits for its hour.
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    tick(&mut pc, &mut g, at);
    pc.fail(Call::EngineStop, "no DriverReleased within 10 s");
    pc.health(Health::Healthy);
    assert_eq!(
        run_switch(&mut pc, &mut g, Mode::Dev, Mode::Event),
        Outcome::KeptServing
    );
    tick(&mut pc, &mut g, at + TICK);
    assert_eq!(pc.count(Call::TuningDrift), 1);
}

// ---- the loop ----

#[test]
fn the_loop_answers_requests_and_ends_on_quit() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    let (tx, rx) = mpsc::channel();
    let (rtx, rrx) = mpsc::sync_channel(1);
    tx.send(Job {
        req: Request::Quit,
        epoch: 0,
        reply: rtx,
    })
    .unwrap();
    let t = Instant::now();
    serve_requests(&mut pc, &mut g, &rx);
    assert!(t.elapsed() < Duration::from_secs(2));
    assert!(rrx.recv().unwrap().ok);
    assert!(g.quit);
    assert!(pc.called(Call::Procs), "the watch ran");
    drop(tx);
    // Without senders it ends at once.
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    let (tx, rx) = mpsc::channel::<Job>();
    drop(tx);
    serve_requests(&mut pc, &mut g, &rx);
    assert!(!g.quit);
    // A hand-over ends it before anything runs.
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    let (_tx, rx) = mpsc::channel::<Job>();
    g.handover = Some(PathBuf::from("iemmixer-guard.exe"));
    serve_requests(&mut pc, &mut g, &rx);
    assert!(pc.calls().is_empty());
}

#[test]
fn the_loop_ends_at_once_when_a_handover_is_already_set() {
    // A bounded, deterministic catch for the `&&`->`||` mutant of the loop
    // condition `while !g.quit && g.handover.is_none()` (daemon.rs:2217). With
    // a handover already set and quit false the condition is
    // `!false && Some.is_none()` = false, so `serve_requests` returns before
    // running a step. The mutant makes it `!false || Some.is_none()` = true, so
    // it loops forever. A sender is kept alive so `recv_timeout` never returns
    // Disconnected (which would end the loop for either version), leaving the
    // loop condition as the only way out. Run it on a thread and require it to
    // end within a bound: the original returns in microseconds; the mutant
    // never returns, so this fails its assertion in ~3 s (a clean FAIL) instead
    // of hanging until nextest's slow-timeout (#23: a hang is only a provisional
    // catch, and this bounded test runs first under `priority = 100`).
    let (_keep, rx) = mpsc::channel::<Job>();
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
        g.handover = Some(PathBuf::from("iemmixer-guard.exe"));
        serve_requests(&mut pc, &mut g, &rx);
        let _ = done_tx.send(());
    });
    assert!(
        done_rx.recv_timeout(Duration::from_secs(3)).is_ok(),
        "serve_requests did not return with a handover already set within 3 s"
    );
}

#[test]
fn the_guard_waits_for_its_last_reply_to_be_written() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Dev));
    let (tx, rx) = mpsc::channel();
    let (rtx, rrx) = mpsc::sync_channel(1);
    tx.send(Job {
        req: Request::Quit,
        epoch: 0,
        reply: rtx,
    })
    .unwrap();
    serve_requests(&mut pc, &mut g, &rx);
    // Handed to the pipe's thread, not yet written.
    let v = g.shared.view();
    assert_eq!((v.replies_sent, v.replies_done), (1, 0));
    let t = Instant::now();
    assert!(!g.shared.await_replies(Duration::from_millis(100)));
    assert!(t.elapsed() >= Duration::from_millis(100));
    // The pipe's thread writes it, then counts it.
    let shared = Arc::clone(&g.shared);
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        assert!(rrx.recv().unwrap().ok);
        shared.reply_done();
    });
    let t = Instant::now();
    assert!(g.shared.await_replies(Duration::from_secs(5)));
    assert!(
        t.elapsed() >= Duration::from_millis(150),
        "{:?}",
        t.elapsed()
    );
    assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
    writer.join().unwrap();
    assert_eq!(g.shared.view().replies_done, 1);
    // Nothing handed over: nothing to wait for.
    let idle = Shared::new(Cancel::default());
    let t = Instant::now();
    assert!(idle.await_replies(Duration::from_secs(5)));
    assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
}

#[test]
fn the_watch_runs_once_a_second() {
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
        serve_requests(&mut pc, &mut g, &rx);
        pc.count(Call::Procs)
    });
    std::thread::sleep(Duration::from_millis(2500));
    let (rtx, rrx) = mpsc::sync_channel(1);
    tx.send(Job {
        req: Request::Quit,
        epoch: 0,
        reply: rtx,
    })
    .unwrap();
    assert!(rrx.recv_timeout(Duration::from_secs(2)).unwrap().ok);
    // At 0, 1 and 2 s; a slow start may lose one, a busy box add none.
    let ticks = worker.join().unwrap();
    assert!((2..=4).contains(&ticks), "{ticks} ticks in 2.5 s");
    assert_eq!(TICK, Duration::from_secs(1));
}

// ---- start, direct ----

fn fixed(t: u64) -> Clock {
    Clock::Fixed(Arc::new(AtomicU64::new(t)))
}

fn save_state(root: &Path, st: &mut GuardState, at: u64) {
    let dir = root.join("guard");
    std::fs::create_dir_all(&dir).unwrap();
    st.save(&dir.join(STATE_FILE), at).unwrap();
}

#[test]
fn a_reboot_resets_the_mode_to_event() {
    let dir = tempfile::tempdir().unwrap();
    let mut saved = GuardState {
        mode: Mode::Dev,
        switching: Some(Switching {
            from: Mode::Event,
            to: Mode::Dev,
            done: vec![Step::Precheck],
            started: 1_000,
        }),
        ..GuardState::default()
    };
    save_state(dir.path(), &mut saved, 1_000);
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    assert_eq!(g.state.mode, Mode::Dev);
    // After the reboot REAPER and the app came up by themselves.
    let mut pc = FakePc::new(band_up());
    assert_eq!(start(&mut pc, &mut g, 5_000), Some(Outcome::Done));
    assert_eq!(g.state.mode, Mode::Event);
    assert_eq!(g.state.switching, None);
    for c in [
        Call::Tuning,
        Call::PrefCheck,
        Call::ReaperFacts,
        Call::AppAnswers,
        Call::Fingerprint,
    ] {
        assert!(pc.called(c), "{c:?}: the event plan's checks");
    }
    assert!(!pc.called(Call::ReaperStart) && !pc.called(Call::EngineStop));
    let (back, err) = GuardState::load(&dir.path().join("guard").join(STATE_FILE));
    assert_eq!(err, None);
    assert_eq!((back.mode, back.switching), (Mode::Event, None));
    assert_eq!(back.written_at, T0);
    // A guard restart (no reboot) with the engine running keeps dev.
    let mut saved = GuardState {
        mode: Mode::Dev,
        ..GuardState::default()
    };
    save_state(dir.path(), &mut saved, 9_000);
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    let mut pc = FakePc::new(iemmixer_up());
    assert_eq!(start(&mut pc, &mut g, 5_000), None);
    assert_eq!(g.state.mode, Mode::Dev);
    assert!(!pc.called(Call::EngineStop));
    assert!(pc.called(Call::Adopt) && pc.called(Call::SetBundle));
    // REAPER running beside our engine (a deployment of the predecessor)
    // is no reboot either: dev stays, the watch alarms.
    let mut saved = GuardState {
        mode: Mode::Dev,
        ..GuardState::default()
    };
    save_state(dir.path(), &mut saved, 9_000);
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    let mut pc = FakePc::new(Facts {
        reaper: true,
        ..iemmixer_up()
    });
    assert_eq!(start(&mut pc, &mut g, 5_000), None);
    assert_eq!(g.state.mode, Mode::Dev);
    // REAPER without our engine is the band's system: event.
    save_state(dir.path(), &mut saved, 9_000);
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    let mut pc = FakePc::new(band_up());
    assert_eq!(start(&mut pc, &mut g, 5_000), Some(Outcome::Done));
    assert_eq!(g.state.mode, Mode::Event);
}

#[test]
fn a_restarted_guard_unwinds_a_half_done_dev_switch() {
    let mut g = Guard::for_test(Mode::Event);
    g.state.switching = Some(Switching {
        from: Mode::Event,
        to: Mode::Dev,
        done: vec![Step::Precheck, Step::AppStop, Step::ReaperSaveQuit],
        started: T0,
    });
    g.state.written_at = 2_000;
    g.state.pins.current = Some(SHA.into());
    let saved_kids = crate::state::Children {
        engine: Some(crate::state::Child {
            pid: 4242,
            start_time: 1,
            image: "iem-engine.exe".into(),
        }),
        ..crate::state::Children::default()
    };
    g.state.pids = saved_kids.clone();
    // REAPER and the app were quit, the engine never came up.
    let mut pc = FakePc::new(Facts::default());
    assert_eq!(start(&mut pc, &mut g, 1_000), Some(Outcome::Done));
    assert_eq!(g.state.mode, Mode::Event);
    assert_eq!(g.state.switching, None);
    assert!(pc.index(Call::ReaperStart) < pc.index(Call::AppStart));
    assert!(!pc.called(Call::EngineStart));
    assert!(pc.index(Call::Adopt) < pc.index(Call::ReaperStart));
    assert_eq!(pc.kids, saved_kids);
    assert_eq!(pc.bundle.as_deref(), Some(SHA));
    // An unfinished event plan resumes the same way.
    let mut g = Guard::for_test(Mode::Dev);
    g.state.switching = Some(Switching {
        from: Mode::Dev,
        to: Mode::Event,
        done: vec![Step::EngineStop],
        started: T0,
    });
    g.state.written_at = 2_000;
    let mut pc = FakePc::new(Facts {
        server: true,
        ..Facts::default()
    });
    assert_eq!(start(&mut pc, &mut g, 1_000), Some(Outcome::Done));
    assert!(pc.index(Call::ServerStop) < pc.index(Call::ReaperStart));
}

#[test]
fn direct_event_runs_without_a_guard() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    let r = direct_event(&mut pc, &mut g, Some(()), false);
    assert!(r.ok, "{r:?}");
    assert!(r.detail.starts_with("direct: event: done"), "{}", r.detail);
    assert_eq!(r.mode, Mode::Event);
    assert!(pc.index(Call::EngineStop) < pc.index(Call::ReaperStart));
    assert!(pc.index(Call::ReaperStart) < pc.index(Call::AppStart));
    assert!(pc.index(Call::Adopt) < pc.index(Call::EngineStop));
    // With the mutex taken it refuses and touches nothing.
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    let r = direct_event::<()>(&mut pc, &mut g, None, false);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "a guard runs; use the pipe")
    );
    assert_eq!(r.mode, Mode::Dev);
    assert!(pc.calls().is_empty());
    // Its dry run shows the plan only.
    let r = direct_event(&mut pc, &mut g, Some(()), true);
    assert!(r.ok);
    assert_eq!(
        r.detail,
        "direct: dry run: EngineStop, ServerStop, TrayStop, TuningExit, PrefCheck, \
         ReaperStart, ReaperHandover, AppStart, AppHandover, Fingerprint"
    );
    assert!(pc.mutating_calls().is_empty());
    assert_eq!(g.state.mode, Mode::Dev);
    // Its alarms go out before it ends.
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    pc.fail(Call::Fingerprint, "the tuning task did not answer");
    let r = direct_event(&mut pc, &mut g, Some(()), false);
    assert!(r.ok);
    assert_eq!(pc.calls().last(), Some(&Call::Notify));
}

// ---- files, install, activate ----

#[test]
fn the_state_and_alarms_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    assert_eq!(g.root(), Some(dir.path()));
    g.raise(Some(Step::PrefCheck), "kept", true);
    g.state.pins.current = Some(SHA.into());
    g.save();
    let back = Guard::open(dir.path(), SiteConf::default(), fixed(T0 + 5));
    assert_eq!(back.state.pins.current.as_deref(), Some(SHA));
    assert_eq!(back.alarms, g.alarms);
    assert_eq!(back.state.written_at, T0);
    assert_eq!(back.shared.view().alarms.len(), 1);
    // Unreadable files start from the defaults with an alarm each.
    let files = dir.path().join("guard");
    std::fs::write(files.join(STATE_FILE), b"{").unwrap();
    std::fs::write(files.join(ALARMS_FILE), b"[").unwrap();
    let g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    assert_eq!(g.state.mode, Mode::Event);
    let t = texts(&g);
    assert_eq!(t.len(), 2);
    assert!(t[0].starts_with("guard state unreadable ("), "{t:?}");
    assert!(t[1].starts_with("guard alarms unreadable ("), "{t:?}");
    assert_eq!(
        (STATE_FILE, ALARMS_FILE),
        ("guard-state.json", "alarms.json")
    );
}

#[test]
fn install_records_the_bundle_and_refuses_other_sums() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    let mut pc = FakePc::new(Facts::default());
    let zip = install::tests::good_zip(dir.path(), SHA);
    let r = handle(
        &mut pc,
        &mut g,
        Request::Install {
            zip: zip.to_string_lossy().into_owned(),
        },
        0,
    );
    assert_eq!(
        (r.ok, r.detail.clone()),
        (true, format!("bundle {SHA} installed"))
    );
    assert_eq!(g.state.bundles[SHA], record(SHA, "dev", Hil::Pending));
    let r = handle(
        &mut pc,
        &mut g,
        Request::Install {
            zip: zip.to_string_lossy().into_owned(),
        },
        0,
    );
    assert_eq!(r.detail, format!("bundle {SHA} already installed"));
    // Other sums for the same SHA: refused with an alarm.
    let mut entries = install::tests::files(SHA);
    if let Some(e) = entries.iter_mut().find(|(n, _)| n == "hil-v1.ps1") {
        e.1 = b"changed".to_vec();
    }
    let sums = install::tests::sums(&entries);
    entries.push((crate::bundle::SUMS.to_owned(), sums.into_bytes()));
    let other = dir.path().join("other.zip");
    install::tests::zip_of(&other, &entries);
    let r = handle(
        &mut pc,
        &mut g,
        Request::Install {
            zip: other.to_string_lossy().into_owned(),
        },
        0,
    );
    let why = format!("bundle {SHA} is installed with other sums; it is never overwritten");
    assert_eq!((r.ok, r.detail.clone()), (false, why.clone()));
    assert_eq!(texts(&g), [why]);
    // A broken zip: refused, no alarm.
    let r = handle(
        &mut pc,
        &mut g,
        Request::Install {
            zip: dir.path().join("none.zip").to_string_lossy().into_owned(),
        },
        0,
    );
    assert!(!r.ok);
    assert!(r.detail.starts_with("install refused: "), "{}", r.detail);
    assert_eq!(g.alarms.all().len(), 1);
    // Without files there is nothing to install into.
    let mut bare = Guard::for_test(Mode::Dev);
    assert_eq!(
        install_bundle(&mut bare, &zip),
        (false, "the guard has no bundle directory".to_owned())
    );
}

#[test]
fn activation_pins_copies_excludes_and_hands_over() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    g.state.mode = Mode::Dev;
    let mut pc = FakePc::new(Facts::default());
    let activate = Request::Activate { sha: SHA.into() };
    let r = handle(&mut pc, &mut g, activate.clone(), 0);
    assert_eq!(r.detail, format!("bundle {SHA} is not installed"));
    let zip = install::tests::good_zip(dir.path(), SHA);
    assert!(install_bundle(&mut g, &zip).0);
    let r = handle(&mut pc, &mut g, activate.clone(), 0);
    assert_eq!(
        (r.ok, r.detail.clone()),
        (
            true,
            format!("activated {SHA}; the guard hands over to its new exe")
        )
    );
    let bin = install::bin_dir(dir.path());
    assert_eq!(g.handover, Some(bin.join(install::GUARD_EXE)));
    assert!(bin.join(install::IEMMODE_EXE).is_file());
    assert_eq!(g.state.pins.current.as_deref(), Some(SHA));
    assert_eq!(pc.bundle.as_deref(), Some(SHA));
    assert!(pc.called(Call::Exclude));
    // The same bundle again: the guard's exe did not change.
    g.handover = None;
    let r = handle(&mut pc, &mut g, activate.clone(), 0);
    assert_eq!((r.ok, r.detail.clone()), (true, format!("activated {SHA}")));
    assert_eq!(g.handover, None);
    // Failed exclusions alarm, the activation stands.
    pc.fail(Call::Exclude, "the exclude task ended with 1");
    let r = handle(&mut pc, &mut g, activate, 0);
    assert!(r.ok);
    assert_eq!(
        texts(&g),
        [format!(
            "Defender exclusions for {SHA}: the exclude task ended with 1"
        )]
    );
    // Without files the activation fails.
    let (mut pc, mut bare) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Dev));
    bare.state
        .bundles
        .insert(SHA.into(), record(SHA, "dev", Hil::Pending));
    let r = handle(&mut pc, &mut bare, Request::Activate { sha: SHA.into() }, 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (
            false,
            "activation failed: the guard has no bundle directory"
        )
    );
    assert!(!pc.called(Call::Exclude));
}

#[test]
fn the_first_offline_install_activates_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    let zip = install::tests::good_zip(dir.path(), SHA);
    assert_eq!(
        install_offline(&mut g, &zip),
        (true, format!("bundle {SHA} installed; activated into bin"))
    );
    assert_eq!(g.state.pins.current.as_deref(), Some(SHA));
    assert!(
        install::bin_dir(dir.path())
            .join(install::GUARD_EXE)
            .is_file()
    );
    // A later one is only installed.
    let zip = install::tests::good_zip(dir.path(), OTHER);
    assert_eq!(
        install_offline(&mut g, &zip),
        (true, format!("bundle {OTHER} installed"))
    );
    assert_eq!(g.state.pins.current.as_deref(), Some(SHA));
    // A refused zip is reported as such.
    let (ok, why) = install_offline(&mut g, &dir.path().join("none.zip"));
    assert!(!ok);
    assert!(why.starts_with("install refused: "), "{why}");
    // The saved state carries both records.
    let back = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    assert_eq!(back.state.bundles.len(), 2);
}

#[test]
fn site_settings_and_clocks() {
    let s = crate::site::tests::settings();
    assert_eq!(
        SiteConf::from_site(&s.guard),
        SiteConf {
            on_pref_fail: PrefFail::StartReaperWithAlarm,
            hil_tx: vec![94, 95],
            prod: false,
        }
    );
    assert_eq!(
        SiteConf::default(),
        SiteConf {
            on_pref_fail: PrefFail::StartReaperWithAlarm,
            hil_tx: Vec::new(),
            prod: false,
        }
    );
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let sys = Clock::System.now();
    assert!((now..=now + 5).contains(&sys), "{sys} {now}");
    assert_eq!(fixed(42).now(), 42);
}

#[test]
fn a_rehearsal_names_a_single_process_that_stays_up() {
    // The teardown's after-check ORs engine/server/tray: a lone stubborn
    // process (here only the server, whose stop is refused) must still be
    // reported. If the first `||` were `&&` it would take TWO up to report any.
    let (mut pc, mut g) = (
        FakePc::new(Facts {
            server: true,
            ..Facts::default()
        }),
        Guard::for_test(Mode::Dev),
    );
    g.state.pins.current = Some(SHA.into());
    pc.fail(Call::ServerStop, "the server did not stop");
    let r = ask(&mut pc, &mut g, Request::RehearseTeardown);
    assert!(!r.ok, "{r:?}");
    assert!(
        r.detail.contains("iemmixer processes still run"),
        "a lone server left up must still be named: {}",
        r.detail
    );
}
