//! The daemon against `FakePc` (S6 plan Task 10 Step 5): the switch runner's
//! pre-emption and error policy, the requests, the watch, the start after a
//! reboot or a restart, and the `--direct` path.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::*;
use crate::bundle::Pins;
use crate::effects::engine::{Ready, ReadyWindow};
use crate::install;
use crate::pc::Status;
use crate::pc::fake::{Call, FakePc};

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
        force: false,
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

// ---- the plan's exact tests ----

#[test]
fn preempt_during_interlock_starts_event_within_1s() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    pc.block_until_cancel(Call::ReaperMeters);
    let c = g.cancel.clone();
    let fired = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        c.preempt();
        Instant::now()
    });
    run_switch(&mut pc, &mut g, Mode::Event, Mode::Dev);
    let at = fired.join().unwrap();
    assert!(!pc.called(Call::AppStop) && !pc.called(Call::ReaperSaveQuit));
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
    pc.block_until_cancel(Call::ReaperMeters);
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
    pc.meters = vec![-60.0, -70.0];
    let r = handle(&mut pc, &mut g, dev(), 0);
    assert!(r.ok, "{r:?}");
    assert!(
        r.detail
            .starts_with("dev: done; interlock quiet: REAPER stage peaks [-60.0, -70.0] dBFS"),
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
            Call::ReaperMeters,
            Call::AppStop,
            Call::ReaperSaveQuit,
            Call::Tuning,
            Call::Data,
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

#[test]
fn a_failed_dev_step_alarms_and_unwinds_to_event() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    pc.meters = vec![-60.0];
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
    pc.reaper.dialog = true;
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
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
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

#[test]
fn the_jobs_are_cancelled_before_the_runner_stops() {
    let (mut pc, mut g) = (
        FakePc::new(Facts {
            runner: true,
            ..iemmixer_up()
        }),
        Guard::for_test(Mode::Dev),
    );
    g.job = Some(4242);
    let r = handle(&mut pc, &mut g, Request::Event { dry_run: false }, 0);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.job, None);
    assert!(r.detail.contains("HIL job 4242 cancelled"), "{}", r.detail);
    assert!(pc.index(Call::RunnerStop) < pc.index(Call::EngineStop));
}

// ---- the interlock ----

#[test]
fn interlock_refusals_retry_every_15_min_and_alarm_once() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    pc.interlock = (false, "activity on mic1".into());
    let r = handle(&mut pc, &mut g, dev(), 0);
    assert!(!r.ok);
    assert_eq!(
        r.detail,
        "dev: refused: activity on stage, tried again every 15 min; the mode is event; \
         the interlock heard the band (activity on mic1); refusal 1"
    );
    assert_eq!(g.state.mode, Mode::Event);
    assert_eq!(g.state.switching, None);
    assert_eq!(
        g.state.interlock_retry,
        Some(InterlockRetry {
            target: Mode::Dev,
            build: None,
            refusals: 1,
            next_at: T0 + RETRY_S,
        })
    );
    assert_eq!(RETRY_S, 900);
    assert_eq!(
        steps(&pc),
        [Call::Precheck, Call::EngineInterlock],
        "nothing was touched"
    );
    assert!(g.alarms.all().is_empty());
    // Not due yet.
    g.set_now(T0 + RETRY_S - 1);
    tick(&mut pc, &mut g, Instant::now());
    assert_eq!(pc.count(Call::EngineInterlock), 1);
    for n in 2..=7 {
        let due = g.state.interlock_retry.as_ref().unwrap().next_at;
        g.set_now(due);
        tick(&mut pc, &mut g, Instant::now());
        let r = g.state.interlock_retry.as_ref().unwrap();
        assert_eq!((r.refusals, r.next_at), (n, due + RETRY_S), "refusal {n}");
        let expected = usize::from(n >= RETRY_NOTICE_AT);
        assert_eq!(g.alarms.all().len(), expected, "refusal {n}");
    }
    let a = g.alarms.last().unwrap();
    assert_eq!(
        (a.text.as_str(), a.step, a.owner_question, a.notified),
        (RETRY_NOTICE, Some(Step::Interlock), false, true)
    );
    // The eighth drops the retry; status says so.
    let due = g.state.interlock_retry.as_ref().unwrap().next_at;
    g.set_now(due);
    tick(&mut pc, &mut g, Instant::now());
    assert_eq!(g.state.interlock_retry, None);
    assert_eq!(pc.count(Call::EngineInterlock), 8);
    assert_eq!(g.alarms.all().len(), 1);
    assert!(
        status_text(&g).contains("the dev switch was dropped after 8 interlock refusals"),
        "{}",
        status_text(&g)
    );
    g.set_now(due + 10 * RETRY_S);
    tick(&mut pc, &mut g, Instant::now());
    assert_eq!(pc.count(Call::EngineInterlock), 8);
    assert_eq!(g.state.mode, Mode::Event);
    assert!(!pc.called(Call::EngineStart));
    assert_eq!((RETRY_NOTICE_AT, RETRY_LAST), (4, 8));
}

#[test]
fn ide_event_or_a_new_request_clears_the_retry() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    pc.interlock = (false, "activity".into());
    handle(&mut pc, &mut g, dev(), 0);
    let due = g.state.interlock_retry.as_ref().unwrap().next_at;
    g.set_now(due);
    tick(&mut pc, &mut g, Instant::now());
    assert_eq!(g.state.interlock_retry.as_ref().unwrap().refusals, 2);
    // A new request starts counting again.
    handle(&mut pc, &mut g, dev(), 0);
    assert_eq!(g.state.interlock_retry.as_ref().unwrap().refusals, 1);
    // "ide event" drops it.
    let r = handle(&mut pc, &mut g, Request::Event { dry_run: false }, 0);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.interlock_retry, None);
    g.set_now(due + 10 * RETRY_S);
    let before = pc.count(Call::EngineInterlock);
    tick(&mut pc, &mut g, Instant::now());
    assert_eq!(pc.count(Call::EngineInterlock), before);
}

#[test]
fn reaper_meters_above_the_activity_level_refuse() {
    for (peaks, quiet) in [
        (vec![-60.0, -50.0], true),
        (vec![-60.0, -49.9], false),
        (vec![f64::NAN], false),
        (vec![], false),
    ] {
        let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
        g.state.pins.current = Some(SHA.into());
        pc.meters = peaks.clone();
        let r = handle(&mut pc, &mut g, dev(), 0);
        assert_eq!(r.ok, quiet, "{peaks:?}: {r:?}");
        assert_eq!(pc.called(Call::AppStop), quiet, "{peaks:?}");
        assert_eq!(g.state.interlock_retry.is_some(), !quiet, "{peaks:?}");
    }
    // A retry keeps its bundle.
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "dev", Hil::Pending));
    pc.meters = vec![-10.0];
    handle(
        &mut pc,
        &mut g,
        Request::Dev {
            build: Some(SHA.into()),
            force: false,
            dry_run: false,
        },
        0,
    );
    assert_eq!(
        g.state.interlock_retry.as_ref().unwrap().build.as_deref(),
        Some(SHA)
    );
}

#[test]
fn stage_quiet_needs_every_peak_at_or_below_the_level() {
    assert!(stage_quiet(&[-50.0]));
    assert!(stage_quiet(&[-90.0, -60.0]));
    assert!(!stage_quiet(&[-49.99]));
    assert!(!stage_quiet(&[-90.0, -30.0]));
    assert!(!stage_quiet(&[f64::NAN]));
    assert!(!stage_quiet(&[]));
}

#[test]
fn force_skips_the_interlock() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    let r = handle(
        &mut pc,
        &mut g,
        Request::Dev {
            build: None,
            force: true,
            dry_run: false,
        },
        0,
    );
    assert!(r.ok, "{r:?}");
    assert!(!pc.called(Call::ReaperMeters));
    // The flags end with the switch: the next one checks the stage again.
    pc.facts = band_up();
    handle(&mut pc, &mut g, Request::Event { dry_run: false }, 0);
    let r = handle(&mut pc, &mut g, dev(), 0);
    assert!(!r.ok);
    assert!(pc.called(Call::ReaperMeters));
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
    assert_eq!(g.job, None);
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
fn the_tray_quits_only_with_a_subscriber() {
    let shared = Shared::new(Cancel::default());
    assert_eq!(
        shared.tray_quit(),
        Err("the tray is not subscribed to the guard".to_owned())
    );
    shared.add_subscriber();
    assert_eq!(shared.tray_quit(), Ok(()));
    assert_eq!(shared.view().tray_quits, 1);
    let seen = (shared.view().version, 0);
    let t = Instant::now();
    let v = shared.wait_change(seen, Duration::from_secs(5));
    assert!(t.elapsed() < Duration::from_secs(1));
    assert_eq!(v.tray_quits, 1);
    shared.drop_subscriber();
    shared.drop_subscriber();
    assert_eq!(shared.view().subscribers, 0);
    assert!(shared.tray_quit().is_err());
    // No change: the wait ends at its limit.
    let v = shared.view();
    let t = Instant::now();
    shared.wait_change((v.version, v.tray_quits), Duration::from_millis(100));
    assert!(t.elapsed() >= Duration::from_millis(100));
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
        force: false,
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
            force: false,
            dry_run: true,
        },
        0,
    );
    assert_eq!(
        r.detail,
        "dry run: Precheck, Interlock, AppStop, ReaperSaveQuit, TuningEnter, Data, EngineStart, \
         EngineArm, ServerStart, TrayStart, IdentityCheck, RunnerStart; bundle none; precheck ok"
    );
    pc.fail(
        Call::Precheck,
        "no alarm recipient: the alarm link was not opened",
    );
    let r = handle(
        &mut pc,
        &mut g,
        Request::Dev {
            build: None,
            force: true,
            dry_run: true,
        },
        0,
    );
    assert!(!r.ok);
    assert!(
        r.detail.ends_with(
            "RunnerStart; bundle none; precheck no alarm recipient: the alarm link was not opened"
        ),
        "{}",
        r.detail
    );
    assert!(!r.detail.contains("Interlock"), "force: {}", r.detail);
    let r = handle(&mut pc, &mut g, Request::Event { dry_run: true }, 0);
    assert_eq!(
        r.detail,
        "dry run: TuningExit, PrefCheck, ReaperHandover, AppHandover, Fingerprint"
    );
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
    assert!(pc.called(Call::EngineInterlock));
    // A trial skips the interlock (the band is there on purpose).
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Green));
    let r = handle(&mut pc, &mut g, live(true), 0);
    assert!(r.ok, "{r:?}");
    assert!(!pc.called(Call::ReaperMeters));
}

#[test]
fn dev_with_a_build_pins_it() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    let build = |sha: &str| Request::Dev {
        build: Some(sha.into()),
        force: false,
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

#[test]
fn job_begin_needs_a_quiet_stage() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    let begin = Request::JobBegin { run: 7 };
    pc.quiet_for = Duration::from_secs(299);
    let r = handle(&mut pc, &mut g, begin.clone(), 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "the band was quiet for 299 s; a job needs 300 s")
    );
    assert!(!pc.called(Call::EngineStagePeaks));
    pc.quiet_for = Duration::from_secs(300);
    pc.stage_peaks = vec![-90.0, -30.0];
    let r = handle(&mut pc, &mut g, begin.clone(), 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "stage peaks [-90.0, -30.0] dBFS: not quiet")
    );
    assert_eq!(g.job, None);
    pc.stage_peaks = vec![-90.0, -50.0];
    let r = handle(&mut pc, &mut g, begin.clone(), 0);
    assert_eq!((r.ok, r.detail.as_str()), (true, "HIL job 7 began"));
    assert_eq!(g.job, Some(7));
    assert_eq!(JOB_PEAKS_S, 60);
    let r = handle(&mut pc, &mut g, Request::JobBegin { run: 8 }, 0);
    assert_eq!(r.detail, "HIL job 7 has not ended");
    // job-end
    let r = handle(&mut pc, &mut g, Request::JobEnd { run: 8 }, 0);
    assert_eq!((r.ok, r.detail.as_str()), (false, "HIL job 7 runs, not 8"));
    let r = handle(&mut pc, &mut g, Request::JobEnd { run: 7 }, 0);
    assert_eq!((r.ok, r.detail.as_str()), (true, "HIL job 7 ended"));
    assert_eq!(g.job, None);
    let r = handle(&mut pc, &mut g, Request::JobEnd { run: 7 }, 0);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (true, "no HIL job runs (job 7 has ended)")
    );
    // Unreadable activity or meters refuse.
    pc.fail(Call::EngineStagePeaks, "no meter frame in 60 s");
    let r = handle(&mut pc, &mut g, begin.clone(), 0);
    assert_eq!(r.detail, "stage peaks: no meter frame in 60 s");
    pc.fail(Call::BandQuietFor, "no topology");
    let r = handle(&mut pc, &mut g, begin.clone(), 0);
    assert_eq!(r.detail, "band activity unreadable: no topology");
    // Only in dev.
    g.state.mode = Mode::Live;
    let r = handle(&mut pc, &mut g, begin, 0);
    assert_eq!(r.detail, "a HIL job is for dev; the mode is live");
}

#[test]
fn the_test_signal_goes_only_to_the_hil_outputs() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
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
            "test signal on mic1 at -20 dBFS for 5 s on card outputs [72]"
        )
    );
    assert_eq!(pc.hil_signals, [("mic1".to_owned(), -20.0, 5.0, vec![72])]);
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
        (Request::RunnerStop, "runner-stop"),
        (Request::RehearseTeardown, "rehearse-teardown"),
        (Request::Activate { sha: SHA.into() }, "activate"),
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
    g.job = Some(3);
    let r = handle(&mut pc, &mut g, Request::RunnerStop, 0);
    assert_eq!(r.detail, "HIL job 3 runs: the runner is not idle");
    assert!(!pc.called(Call::RunnerStop));
    g.job = None;
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
fn rehearse_teardown_never_starts_reaper() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    g.state.pins.current = Some(SHA.into());
    let r = handle(&mut pc, &mut g, Request::RehearseTeardown, 0);
    assert!(r.ok, "{r:?}");
    assert!(
        r.detail.starts_with(
            "teardown clean: module unheld, preference original, ports free; dev: done"
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
    assert_eq!(pc.count(Call::PrefCheck), 2);
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
    let r = handle(&mut pc, &mut g, Request::RehearseTeardown, 0);
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
    let r = handle(&mut pc, &mut g, Request::RehearseTeardown, 0);
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
    let r = handle(&mut pc, &mut g, dev(), 0);
    assert!(!r.ok);
    assert!(pc.called(Call::ReaperStart));
    // A healthy engine that does not release stops the rehearsal.
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    pc.fail(Call::EngineStop, "no DriverReleased within 10 s");
    pc.health(Health::Healthy);
    let r = handle(&mut pc, &mut g, Request::RehearseTeardown, 0);
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
    let r = handle(&mut pc, &mut g, Request::RehearseTeardown, 0);
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
        (true, "the test alarm reached the alarm recipients")
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
    pc.fail(Call::Notify, "no recipients");
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
    g.job = Some(4242);
    g.state.interlock_retry = Some(InterlockRetry {
        target: Mode::Live,
        build: None,
        refusals: 2,
        next_at: T0 + 900,
    });
    g.note = Some("a note".into());
    g.raise(None, "one", false);
    g.raise(None, "two", false);
    assert_eq!(
        status_text(&g),
        format!(
            "mode dev; bundle {SHA}; HIL job 4242; the live switch waits: 2 interlock \
             refusals, next try at 1790000900; a note; 2 unacknowledged alarms"
        )
    );
    assert_eq!(g.shared.view().status, status_text(&g));
    assert_eq!(
        [Mode::Event, Mode::Dev, Mode::Live].map(mode_name),
        ["event", "dev", "live"]
    );
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
fn notices_go_to_the_alarm_recipients_once() {
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
        interlock_retry: Some(InterlockRetry {
            target: Mode::Dev,
            build: None,
            refusals: 1,
            next_at: 2_000,
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
    assert_eq!(g.state.interlock_retry, None);
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
        done: vec![
            Step::Precheck,
            Step::Interlock,
            Step::AppStop,
            Step::ReaperSaveQuit,
        ],
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
            hil_tx: vec![71, 72],
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
