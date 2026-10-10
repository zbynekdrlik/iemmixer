//! The daemon against `FakePc` (S6 plan Task 10 Step 5): the shared
//! fixtures, the plan's exact pre-emption tests, the watch and the request
//! loop. `.config/nextest.toml` names three bounded tests here by their path
//! (`daemon::tests::…`, run first under the mutants profile), so they stay
//! in this module. The other daemon tests are in the sibling `*_tests.rs`
//! files, by the part of the daemon they test.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::*;
use crate::bundle::{Hil, Record};
use crate::lifecycle::{Lifecycle, Prod};
use crate::pc::Kid;
use crate::pc::fake::{Call, FakePc};
use crate::plan::{Facts, Health};
use crate::proto::{Reply, Request};

pub(super) const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
pub(super) const OTHER: &str = "89abcdef0123456789abcdef0123456789abcdef";
pub(super) const T0: u64 = 1_790_000_000;
/// The generation of a guard that began no switch and routed no "ide event".
pub(super) const INIT: Generation = Generation { epoch: 0, fence: 0 };

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
        ..Facts::default()
    }
}

pub(super) fn dev() -> Request {
    Request::Dev {
        build: None,
        dry_run: false,
    }
}

pub(super) fn record(sha: &str, branch: &str, hil: Hil) -> Record {
    Record {
        sha: sha.to_owned(),
        branch: branch.to_owned(),
        run: 4242,
        installed_at: T0,
        hil,
    }
}

/// Prod (S8, #11) on the pin `SHA`, `previous` before it, `maintenance`
/// running: test-only state (nothing in lane 1 sets prod).
pub(super) fn prod_on(previous: Option<&str>, maintenance: Option<&str>) -> Lifecycle {
    Lifecycle::Prod(Prod {
        since: T0,
        pin: SHA.into(),
        previous: previous.map(str::to_owned),
        maintenance: maintenance.map(str::to_owned),
    })
}

/// The calls without the reads every step makes (facts, children).
pub(super) fn steps(pc: &FakePc) -> Vec<Call> {
    pc.calls()
        .into_iter()
        .filter(|c| !matches!(c, Call::Facts | Call::Children))
        .collect()
}

pub(super) fn texts(g: &Guard) -> Vec<String> {
    g.alarms.iter().map(|a| a.text.clone()).collect()
}

/// A request sent after the one before it was answered: the pipe queues it
/// with the switch generation of that moment (`Shared::route`). A literal
/// generation after a switch began (a request, the watch's crash fallback) is a
/// request queued before that switch, answered as during it (`stale`; a dev or
/// live entry only once the fence moved, #42).
pub(super) fn ask(pc: &mut FakePc, g: &mut Guard, req: Request) -> Reply {
    let seen = g.shared.generation();
    handle(pc, g, req, seen)
}

/// What `iemmode status` gets: the view's reply, answered by the pipe.
pub(super) fn status_reply(g: &Guard) -> String {
    match g.shared.route(&Request::Status) {
        Route::Now(r) => r.detail,
        other => panic!("{other:?}"),
    }
}

pub(super) fn fixed(t: u64) -> Clock {
    Clock::Fixed(Arc::new(AtomicU64::new(t)))
}

/// What the event plan's check says of REAPER holding the card at 32.
pub(super) const HELD: &str =
    "REAPER runs with the preferred buffer at 32; it is restored at REAPER's next start";

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
    let r = handle(&mut pc, &mut g, Request::Event { dry_run: false }, INIT);
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

/// Pre-empts from another thread 50 ms after `step` was published as done.
pub(super) fn preempt_after(g: &Guard, step: Step) -> std::thread::JoinHandle<()> {
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
fn an_event_unwind_does_not_recurse_when_a_step_is_preempted() {
    // A bounded, deterministic catch for the `to != Mode::Event` -> `true`
    // mutant of the preempted-step arm (`daemon/runner.rs`, `switch`). In an event plan
    // (to == Event) the real code treats a preempted step as an ordinary
    // event-plan failure and goes on, so `run_switch` returns after a
    // single ReaperFacts read (NeedsOwner: a handover cut short is no done,
    // #10). The mutant makes the guard `true`, so the
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
        // The handover step waits on the token; a pre-emption ends its wait.
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
        Ok((Outcome::NeedsOwner, 1)),
        "an event switch recursed into back_to_event on a preempted step \
         instead of continuing the plan after one handover (or did not end \
         within 3 s)"
    );
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
    // (`daemon/watch.rs`). With no engine process left the real loop never runs
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
    let r = handle(&mut pc, &mut g, Request::Event { dry_run: false }, INIT);
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
    // S8 (#11): prod and its pins are the lifecycle's (`lifecycle::crash_loop`).
    let prod = |previous: Option<&str>| prod_on(previous, None);
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Live));
    g.state.lifecycle = prod(Some(OTHER));
    // The previous pin is a green main build (G8, `lifecycle::crash_loop`).
    g.state
        .bundles
        .insert(OTHER.into(), record(OTHER, "main", Hil::Green));
    pc.exited = vec![(Kid::Engine, Some(70)); 3];
    tick(&mut pc, &mut g, Instant::now());
    assert_eq!(g.state.mode, Mode::Live);
    assert_eq!(
        g.state.lifecycle,
        Lifecycle::Prod(Prod {
            since: T0,
            pin: OTHER.into(),
            previous: None,
            maintenance: None,
        })
    );
    assert_eq!(g.state.active_bundle(), Some(OTHER));
    assert_eq!(pc.bundle.as_deref(), Some(OTHER));
    assert_eq!(pc.count(Call::EngineStart), 1);
    assert_eq!(
        texts(&g),
        [format!(
            "the engine crashed 3 times in 10 min: back to the previous pin {OTHER}"
        )]
    );
    // Without a previous pin it alarms, naming the rollback, and stays down.
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Live));
    g.state.lifecycle = prod(None);
    pc.exited = vec![(Kid::Engine, Some(70)); 3];
    tick(&mut pc, &mut g, Instant::now());
    assert_eq!(
        texts(&g),
        [format!(
            "the engine crashed 3 times in 10 min on the pin {SHA}, and no previous pin is \
             left: the engine stays down; roll back to REAPER (iemmode rollback)"
        )]
    );
    assert_eq!(g.state.lifecycle, prod(None));
    tick(&mut pc, &mut g, Instant::now() + Duration::from_secs(20));
    assert!(!pc.called(Call::EngineStart));
    assert_eq!(g.state.mode, Mode::Live);
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
        generation: INIT,
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
    // condition `while !g.quit && g.handover.is_none()` (`daemon.rs`). With
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
        generation: INIT,
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
        generation: INIT,
        reply: rtx,
    })
    .unwrap();
    assert!(rrx.recv_timeout(Duration::from_secs(2)).unwrap().ok);
    // At 0, 1 and 2 s; a slow start may lose one, a busy box add none.
    let ticks = worker.join().unwrap();
    assert!((2..=4).contains(&ticks), "{ticks} ticks in 2.5 s");
    assert_eq!(TICK, Duration::from_secs(1));
}
