//! In prod nothing goes to REAPER on its own (the owner's decision on #11,
//! 2026-10-19): what a dev or live entry that stops leaves, against
//! `FakePc` (the review of PR #56). An entry whose engine armed stays in its
//! target with that engine; one that stopped or started what plays the band
//! (or sent such a stop) ends in event, where the crash watch respawns
//! nothing and both ways on run from; one that changed nothing stays in the
//! mode it began from. The same rule holds for an entry a restarted guard
//! finds cut off, the step it was in being unknown. The entry's build
//! becomes the active bundle only once its engine starts.

use std::time::{Duration, Instant};

use super::tests::{
    BOOT, INIT, OTHER, SHA, T0, ask, booted_in_prod, iemmixer_up, no_reaper, prod_on, record, texts,
};
use super::*;
use crate::bundle::{Hil, Pins};
use crate::crash;
use crate::lifecycle::{Lifecycle, Prod};
use crate::pc::Kid;
use crate::pc::fake::{Call, FakePc};
use crate::plan::{Facts, Health};
use crate::proto::Request;
use crate::state::Switching;

/// Prod live on the pin `SHA` (a green main build) with its engine, server
/// and tray up; `OTHER` installed, a dev build HIL has not judged (it may
/// not go live, G8).
fn prod_live() -> (FakePc, Guard) {
    let pc = FakePc::new(iemmixer_up());
    let mut g = Guard::for_test(Mode::Live);
    g.state.lifecycle = prod_on(None, None);
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Green));
    g.state
        .bundles
        .insert(OTHER.into(), record(OTHER, "dev", Hil::Pending));
    g.state.set_active(SHA);
    g.save();
    (pc, g)
}

/// `iemmode event`: the owner's "ide event" with `signal`, else the button.
fn event(signal: bool) -> Request {
    Request::Event {
        dry_run: false,
        signal,
    }
}

/// `iemmode dev --build OTHER`: maintenance on a dev build.
fn dev_other() -> Request {
    Request::Dev {
        build: Some(OTHER.into()),
        dry_run: false,
    }
}

/// The review of PR #56, finding 1: prod live on the pin, `iemmode dev
/// --build OTHER` stopped the pin's engine, server and tray and started
/// OTHER's engine, held, whose arm failed. The entry stopped there, but in
/// live: "ide event" then read the held engine's advancing callbacks as a
/// healthy band system and left it ("nothing switched") while nothing
/// played, and the held engine's crash respawned OTHER, unheld, in live (a
/// build that may not go live, G8). Once a step changed what runs the entry
/// ends in event: the crash watch respawns nothing there, and "ide event"
/// goes live on the pin.
#[test]
fn a_prod_entry_from_live_that_stops_after_its_stops_ends_in_event() {
    let held = || {
        let (mut pc, mut g) = prod_live();
        pc.fail(Call::EngineReady, "the engine faulted");
        let r = handle(&mut pc, &mut g, dev_other(), INIT);
        assert!(!r.ok, "{r:?}");
        pc.heal(Call::EngineReady);
        (pc, g)
    };
    let (mut pc, mut g) = held();
    assert_eq!(
        (g.state.mode, g.state.switching.clone()),
        (Mode::Event, None)
    );
    assert!(pc.facts.engine, "the held engine runs");
    assert!(!pc.called(Call::ReaperStart) && !pc.called(Call::AppStart));
    assert_eq!(
        texts(&g),
        [format!("EngineArm: the engine faulted; {}", no_reaper(SHA))]
    );
    assert!(g.alarms.last().unwrap().owner_question);
    assert_eq!(g.state.lifecycle, prod_on(None, None));
    // The held engine ends: nothing starts again.
    pc.exited = vec![(Kid::Engine, Some(70))];
    pc.facts.engine = false;
    let at = Instant::now();
    tick(&mut pc, &mut g, at);
    tick(&mut pc, &mut g, at + Duration::from_secs(20));
    assert_eq!(pc.engine_starts, [(true, false)], "no respawn");
    assert_eq!(g.state.mode, Mode::Event);
    // "ide event": live on the pin. From event it reads no health, so the
    // held engine never passes for the band's system here, whatever its
    // health would read (`effects::engine::health` reads a held engine as
    // never healthy: `a_held_engine_is_never_healthy`).
    let (mut pc, mut g) = held();
    pc.health(Health::Healthy);
    let r = ask(&mut pc, &mut g, event(true));
    assert!(r.ok, "{r:?}");
    assert!(!r.detail.contains("nothing switched"), "{}", r.detail);
    assert_eq!(g.state.mode, Mode::Live);
    assert_eq!(pc.engine_starts, [(true, false); 2]);
    assert_eq!(pc.bundle.as_deref(), Some(SHA));
    assert_eq!(g.state.active_bundle(), Some(SHA));
    assert!(!pc.called(Call::ReaperStart) && !pc.called(Call::AppStart));
}

/// The same rule for an entry a restarted guard (no reboot) finds cut off
/// in prod (the review of PR #56, finding 1): the step it was in is
/// unknown, so once any step finished it stops in event (a HIL job, which
/// lives in dev only, ends with dev); cut off in its precheck, the first,
/// it stays in the mode it began from. Nothing starts and nothing stops
/// either way.
#[test]
fn a_cut_off_prod_entry_ends_in_event_once_a_step_changed_what_runs() {
    let left = |from: Mode, to: Mode, done: Vec<Step>| {
        let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(from));
        g.state.lifecycle = prod_on(None, None);
        g.state
            .bundles
            .insert(SHA.into(), record(SHA, "main", Hil::Green));
        g.state.job = (from == Mode::Dev).then_some(7);
        g.state.switching = Some(Switching {
            from,
            to,
            done,
            started: T0,
        });
        assert_eq!(start(&mut pc, &mut g, 0), None);
        for c in [
            Call::EngineStop,
            Call::EngineStart,
            Call::ReaperStart,
            Call::AppStart,
        ] {
            assert!(!pc.called(c), "{c:?}");
        }
        assert_eq!(g.state.switching, None);
        assert_eq!(g.shared.view().switching, None);
        assert!(g.alarms.last().unwrap().owner_question);
        g
    };
    for (from, to) in [
        (Mode::Live, Mode::Dev),
        (Mode::Dev, Mode::Live),
        (Mode::Live, Mode::Live),
        (Mode::Event, Mode::Live),
    ] {
        for done in [vec![Step::Precheck, Step::EngineStop], vec![Step::Precheck]] {
            let g = left(from, to, done.clone());
            assert_eq!(g.state.mode, Mode::Event, "{from:?} → {to:?} {done:?}");
            assert_eq!(g.shared.view().mode, Mode::Event, "{from:?} → {to:?}");
            assert_eq!(g.state.job, None, "{from:?} → {to:?} {done:?}");
        }
        let g = left(from, to, Vec::new());
        assert_eq!(g.state.mode, from, "{from:?} → {to:?}");
        assert_eq!(
            g.state.job,
            (from == Mode::Dev).then_some(7),
            "{from:?} → {to:?}"
        );
    }
}

/// The active bundle, its way back and the legacy pins as they stand.
fn actives(g: &Guard) -> (Option<String>, Option<String>, Pins) {
    (
        g.state.active.clone(),
        g.state.way_back.clone(),
        g.state.pins.clone(),
    )
}

/// The review of PR #56, finding 3: an entry made its build the active
/// bundle (and `pc`'s) before its first step, so a prod entry from live that
/// stopped before it changed anything (its precheck) left the pin's engine
/// playing in live with another build active: that engine's next crash
/// respawned the other build unheld in live (G8), and the status and the
/// identity check named it. The entry's build becomes the active bundle
/// once its engine starts; until then the active bundle, its way back and
/// the legacy pins stay as they were, and `pc` runs the active bundle
/// again.
#[test]
fn an_entry_s_build_becomes_the_active_bundle_only_once_its_engine_starts() {
    let (mut pc, mut g) = prod_live();
    let before = actives(&g);
    pc.fail(Call::Precheck, "no PWA notification subscription");
    let r = handle(&mut pc, &mut g, dev_other(), INIT);
    assert!(!r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Live);
    assert_eq!(actives(&g), before);
    assert_eq!(pc.bundle.as_deref(), Some(SHA));
    let status = status_text(&g);
    assert!(status.contains(&format!("bundle {SHA}")), "{status}");
    // The pin's engine ends: the pin's engine again, unheld.
    pc.exited = vec![(Kid::Engine, Some(70))];
    pc.facts.engine = false;
    let at = Instant::now();
    tick(&mut pc, &mut g, at);
    tick(&mut pc, &mut g, at + Duration::from_secs(20));
    assert_eq!(pc.engine_starts, [(false, false)]);
    assert_eq!(pc.bundle.as_deref(), Some(SHA));
    // Stopped at the old engine's stop, or after it, before its own engine
    // started: in event, and the active bundle stays.
    for (call, why) in [
        (Call::EngineStop, "no DriverReleased within 10 s"),
        (Call::PrefCheck, "3 restores failed"),
    ] {
        let (mut pc, mut g) = prod_live();
        let before = actives(&g);
        pc.fail(call, why);
        let r = handle(&mut pc, &mut g, dev_other(), INIT);
        assert!(!r.ok, "{call:?}: {r:?}");
        assert_eq!(g.state.mode, Mode::Event, "{call:?}");
        assert!(pc.engine_starts.is_empty(), "{call:?}");
        assert_eq!(actives(&g), before, "{call:?}");
        assert_eq!(pc.bundle.as_deref(), Some(SHA), "{call:?}");
    }
    // Its engine started, held, and did not arm: that build runs, so it is
    // the active bundle, the pin the way back.
    let (mut pc, mut g) = prod_live();
    pc.fail(Call::EngineReady, "the engine faulted");
    let r = handle(&mut pc, &mut g, dev_other(), INIT);
    assert!(!r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Event);
    assert_eq!(
        (g.state.active_bundle(), g.state.way_back_bundle()),
        (Some(OTHER), Some(SHA))
    );
    assert_eq!(pc.bundle.as_deref(), Some(OTHER));
    // An entry that ran: its build is the active bundle.
    let (mut pc, mut g) = prod_live();
    let r = handle(&mut pc, &mut g, dev_other(), INIT);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Dev);
    assert_eq!(
        (g.state.active_bundle(), g.state.way_back_bundle()),
        (Some(OTHER), Some(SHA))
    );
    assert_eq!(pc.bundle.as_deref(), Some(OTHER));
}

/// The review of PR #56, finding 4: only a finished step saved the guard's
/// children (`Guard::done`), so an engine `EngineArm` started again (the
/// boot's attempts, the busy restart) was not saved when the step then
/// failed: the saved pids named the engine stopped before it, and a guard
/// restarted without a reboot found the engine that runs foreign (its
/// precheck refuses every entry). Each start inside the step is saved.
#[test]
fn an_engine_started_again_inside_the_arm_is_saved_as_the_guard_s_child() {
    let (mut pc, mut g) = booted_in_prod();
    pc.ready_misses = vec![1; 4];
    assert_eq!(start(&mut pc, &mut g, BOOT), Some(Outcome::NeedsOwner));
    assert_eq!(pc.engine_starts.len(), 4);
    assert_eq!(g.state.pids.engine.as_ref().map(|k| k.pid), Some(1004));
    assert_eq!(g.state.pids, pc.kids);
    // A prod live entry (one attempt): its engine ended busy, was started
    // again, and missed periods.
    let (mut pc, mut g) = prod_live();
    pc.early_exits = vec![Some(crash::STATE_BUSY)];
    pc.ready_misses = vec![1];
    let live = Request::Live {
        build: None,
        trial: false,
        dry_run: false,
    };
    let r = handle(&mut pc, &mut g, live, INIT);
    assert!(!r.ok, "{r:?}");
    assert_eq!(pc.engine_starts.len(), 2);
    assert_eq!(g.state.pids.engine.as_ref().map(|k| k.pid), Some(1002));
    assert_eq!(g.state.pids, pc.kids);
}

/// The review of PR #56, finding 5: in prod a guard restarted without a
/// reboot, with the band's system up and no engine, reset the state before
/// it looked for a left entry, so the owner's "ide event" (a live entry
/// that had stopped the app when the guard died, REAPER not yet quit)
/// became the event plan: the app started again, REAPER kept, all on its
/// own. A left dev or live entry is held first (`hold_cut_off`, in event
/// here: the app stopped), and the owner's ways on run from there; an event
/// plan that was left resumes, a restart with none left runs the event
/// checks as before, a reboot goes live on the pin, and before the cutover
/// a left entry unwinds as before.
#[test]
fn in_prod_a_guard_restart_with_reaper_up_holds_a_left_entry() {
    let restarted = |lifecycle: Lifecycle, left: Option<Switching>| {
        // REAPER runs on the card; the app is gone.
        let pc = FakePc::new(Facts {
            reaper: true,
            reaper_holds_module: true,
            ..Facts::default()
        });
        let mut g = Guard::for_test(Mode::Event);
        g.state.lifecycle = lifecycle;
        g.state
            .bundles
            .insert(SHA.into(), record(SHA, "main", Hil::Green));
        g.state.switching = left;
        (pc, g)
    };
    let entry = Switching {
        from: Mode::Event,
        to: Mode::Live,
        done: vec![Step::Precheck, Step::AppStop],
        started: T0,
    };
    let prod = || prod_on(None, None);
    let (mut pc, mut g) = restarted(prod(), Some(entry.clone()));
    assert_eq!(start(&mut pc, &mut g, 0), None);
    assert_eq!(
        (g.state.mode, g.state.switching.clone()),
        (Mode::Event, None)
    );
    for c in [
        Call::ReaperSaveQuit,
        Call::ReaperStart,
        Call::AppStop,
        Call::AppStart,
        Call::EngineStart,
    ] {
        assert!(!pc.called(c), "{c:?}");
    }
    assert_eq!(
        texts(&g),
        [format!(
            "the guard started again during the switch event → live (2 steps done); {}",
            no_reaper(SHA)
        )]
    );
    assert!(g.alarms.last().unwrap().owner_question);
    // "ide event" from there: live on the pin, REAPER saved and quit.
    let r = ask(&mut pc, &mut g, event(true));
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Live);
    assert!(pc.called(Call::ReaperSaveQuit) && pc.called(Call::EngineStart));
    assert!(!pc.called(Call::ReaperStart) && !pc.called(Call::AppStart));
    // An event plan that was left resumes: it was asked for.
    let plan = Switching {
        from: Mode::Live,
        to: Mode::Event,
        done: vec![Step::EngineStop],
        started: T0,
    };
    let (mut pc, mut g) = restarted(prod(), Some(plan));
    assert!(start(&mut pc, &mut g, 0).is_some());
    assert_eq!(g.state.mode, Mode::Event);
    assert!(pc.called(Call::AppStart));
    // None left: the event checks as before.
    let (mut pc, mut g) = restarted(prod(), None);
    assert!(start(&mut pc, &mut g, 0).is_some());
    assert!(pc.called(Call::AppStart));
    assert!(g.alarms.all().is_empty(), "{:?}", texts(&g));
    // A reboot: the boot's live entry on the pin, whatever was left.
    let (mut pc, mut g) = restarted(prod(), Some(entry.clone()));
    g.state.written_at = 1_000;
    assert_eq!(start(&mut pc, &mut g, BOOT), Some(Outcome::Done));
    assert_eq!(g.state.mode, Mode::Live);
    assert!(!pc.called(Call::AppStart) && !pc.called(Call::ReaperStart));
    // Before the cutover a left entry unwinds to the event plan.
    let (mut pc, mut g) = restarted(Lifecycle::Trial, Some(entry));
    assert!(start(&mut pc, &mut g, 0).is_some());
    assert_eq!(g.state.mode, Mode::Event);
    assert!(pc.called(Call::AppStart));
}

/// What a prod alarm says after a hold from live that changed nothing.
const PLAYS_ON: &str = "in prod nothing goes to REAPER on its own: no step done changed what \
                        runs, so the PC stays in live as it was (asking for it again tries \
                        again)";

/// …and after one from dev (maintenance).
const RUNS_ON: &str = "in prod nothing goes to REAPER on its own: no step done changed what \
                       runs, so the PC stays in dev as it was (asking for it again tries again)";

/// The review of PR #56, finding 6: after a hold that changed nothing (the
/// PC in the mode it began from, the old engine still playing) the alarm
/// named `iemmode live --build <pin>` as the way on, which would restart a
/// healthy engine. Such an alarm says the PC stays as it was and that
/// asking again tries again, never that anything plays (the engine may be
/// one that does not); a hold from event, and one that changed what runs
/// (now in event, nothing of iemmixer playing), keep the ways on.
#[test]
fn an_alarm_after_a_hold_that_changed_nothing_says_the_pc_stays_as_it_was() {
    const REFUSED: &str = "no PWA notification subscription";
    let live = || Request::Live {
        build: None,
        trial: false,
        dry_run: false,
    };
    let (mut pc, mut g) = prod_live();
    pc.fail(Call::Precheck, REFUSED);
    assert!(!handle(&mut pc, &mut g, dev_other(), INIT).ok);
    assert_eq!(g.state.mode, Mode::Live);
    assert_eq!(texts(&g), [format!("Precheck: {REFUSED}; {PLAYS_ON}")]);
    // From dev, the live entry that would end maintenance.
    let (mut pc, mut g) = prod_live();
    g.state.mode = Mode::Dev;
    pc.fail(Call::Precheck, REFUSED);
    assert!(!handle(&mut pc, &mut g, live(), INIT).ok);
    assert_eq!(g.state.mode, Mode::Dev);
    assert_eq!(texts(&g), [format!("Precheck: {REFUSED}; {RUNS_ON}")]);
    // The HIL job and its runner are nothing that plays: a runner that did
    // not stop changed nothing either, and the job it still runs is kept.
    let (mut pc, mut g) = prod_live();
    g.state.mode = Mode::Dev;
    g.state.job = Some(7);
    pc.facts.runner = true;
    pc.fail(Call::RunnerStop, "the runner did not stop");
    assert!(!handle(&mut pc, &mut g, live(), INIT).ok);
    assert_eq!((g.state.mode, g.state.job), (Mode::Dev, Some(7)));
    assert!(pc.facts.engine && !pc.called(Call::EngineStop));
    assert_eq!(
        texts(&g),
        [format!("RunnerStop: the runner did not stop; {RUNS_ON}")]
    );
    // From event nothing of iemmixer played: the ways on.
    let (mut pc, mut g) = prod_live();
    g.state.mode = Mode::Event;
    pc.facts = Facts::default();
    pc.fail(Call::Precheck, REFUSED);
    assert!(!handle(&mut pc, &mut g, live(), INIT).ok);
    assert_eq!(g.state.mode, Mode::Event);
    assert_eq!(
        texts(&g),
        [format!("Precheck: {REFUSED}; {}", no_reaper(SHA))]
    );
    // A cut-off entry from live that changed nothing (cut off in its
    // precheck) says the same.
    let (mut pc, mut g) = prod_live();
    g.state.switching = Some(Switching {
        from: Mode::Live,
        to: Mode::Dev,
        done: Vec::new(),
        started: T0,
    });
    g.state.written_at = 0;
    assert_eq!(start(&mut pc, &mut g, 0), None);
    assert_eq!(g.state.mode, Mode::Live);
    assert_eq!(
        texts(&g),
        [format!(
            "the guard started again during the switch live → dev (0 steps done); {PLAYS_ON}"
        )]
    );
}

/// The review of the fix of PR #56's finding 1: a prod entry whose engine
/// was armed before a later step failed (the server, the tray, the
/// identity check, the runner) has that engine playing. It stays in the
/// entry's target with it, where the crash watch respawns it and "ide
/// event" finds the band's system; the entry's lifecycle change holds (the
/// maintenance build that became the pin, a maintenance session's build);
/// its alarm says so. In prod a live entry's engine is the pin's (G8).
#[test]
fn a_prod_hold_after_its_engine_armed_keeps_that_engine_in_the_entry_s_mode() {
    const HELD: &str = "ports 80/443 are still held";
    let armed = |m: &str| {
        let ways = if m == "live" {
            format!(
                " (ide event keeps it as it is): iemmode live --build {SHA} starts it all \
                 again, iemmode event starts REAPER (the PC stays prod)"
            )
        } else {
            " (asking for it again tries again)".to_owned()
        };
        [format!(
            "ServerStart: {HELD}; in prod nothing goes to REAPER on its own: its engine was \
             armed, so the PC is in {m} with it{ways}"
        )]
    };
    let live = || Request::Live {
        build: None,
        trial: false,
        dry_run: false,
    };
    // Live again on the pin.
    let (mut pc, mut g) = prod_live();
    pc.fail(Call::ServerStart, HELD);
    assert!(!handle(&mut pc, &mut g, live(), INIT).ok);
    assert_eq!(g.state.mode, Mode::Live);
    assert_eq!(texts(&g), armed("live"));
    assert_eq!(
        (g.state.active_bundle(), pc.bundle.as_deref()),
        (Some(SHA), Some(SHA))
    );
    assert_eq!(g.state.lifecycle, prod_on(None, None));
    pc.health(Health::Healthy);
    let r = ask(&mut pc, &mut g, event(true));
    assert!(r.ok && r.detail.contains("nothing switched"), "{r:?}");
    pc.exited = vec![(Kid::Engine, Some(70))];
    pc.facts.engine = false;
    let at = Instant::now();
    tick(&mut pc, &mut g, at);
    tick(&mut pc, &mut g, at + Duration::from_secs(20));
    assert_eq!(pc.engine_starts, [(true, false), (false, false)]);
    assert_eq!(pc.bundle.as_deref(), Some(SHA));
    // The live entry that ends maintenance on a green main build: that
    // build plays, so it is the pin.
    let (mut pc, mut g) = prod_live();
    g.state.mode = Mode::Dev;
    g.state
        .bundles
        .insert(OTHER.into(), record(OTHER, "main", Hil::Green));
    g.state.lifecycle = prod_on(None, Some(OTHER));
    pc.fail(Call::ServerStart, HELD);
    assert!(!handle(&mut pc, &mut g, live(), INIT).ok);
    assert_eq!(g.state.mode, Mode::Live);
    assert_eq!(
        g.state.lifecycle,
        Lifecycle::Prod(Prod {
            since: T0,
            pin: OTHER.into(),
            previous: Some(SHA.into()),
            maintenance: None,
        })
    );
    assert_eq!(g.state.active_bundle(), Some(OTHER));
    // Maintenance on a dev build: its engine plays in dev, the session's
    // build is recorded.
    let (mut pc, mut g) = prod_live();
    pc.fail(Call::ServerStart, HELD);
    assert!(!handle(&mut pc, &mut g, dev_other(), INIT).ok);
    assert_eq!(g.state.mode, Mode::Dev);
    assert_eq!(texts(&g), armed("dev"));
    assert_eq!(g.state.lifecycle, prod_on(None, Some(OTHER)));
    assert_eq!(g.state.active_bundle(), Some(OTHER));
    // The boot's live entry: live with the pin's engine.
    let (mut pc, mut g) = booted_in_prod();
    pc.fail(Call::ServerStart, HELD);
    assert_eq!(start(&mut pc, &mut g, BOOT), Some(Outcome::NeedsOwner));
    assert_eq!(g.state.mode, Mode::Live);
    assert_eq!(texts(&g), armed("live"));
    assert!(!pc.called(Call::ReaperStart) && !pc.called(Call::AppStart));
}

/// The second review of the fixes for PR #56: a restarted guard that finds
/// a prod entry cut off after its engine armed is in the entry's target
/// with that engine only when it adopted an engine (else nothing of it
/// runs: event). A live entry's lifecycle change then holds as for a failed
/// one (here the live entry that ends maintenance on a green main build
/// makes it the pin, and says so); a dev entry's build is not known after
/// the cut (a plain `dev` keeps the session), so dev keeps the lifecycle
/// (the third review).
#[test]
fn a_cut_off_prod_entry_after_its_engine_armed_is_in_with_the_adopted_engine() {
    let cut_off = |adopted: bool| {
        let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
        g.state.lifecycle = prod_on(None, Some(OTHER));
        for sha in [SHA, OTHER] {
            g.state
                .bundles
                .insert(sha.into(), record(sha, "main", Hil::Green));
        }
        g.state.set_active(OTHER);
        g.state.pids.engine = adopted.then(|| crate::state::Child {
            pid: 4242,
            start_time: 1,
            image: "iem-engine.exe".into(),
        });
        g.state.switching = Some(Switching {
            from: Mode::Dev,
            to: Mode::Live,
            done: vec![
                Step::Precheck,
                Step::EngineStop,
                Step::ServerStop,
                Step::TrayStop,
                Step::TuningEnter,
                Step::PrefCheck,
                Step::EngineStart,
                Step::EngineArm,
            ],
            started: T0,
        });
        assert_eq!(start(&mut pc, &mut g, 0), None);
        assert!(!pc.called(Call::EngineStart) && !pc.called(Call::ReaperStart));
        assert!(g.alarms.last().unwrap().owner_question);
        g
    };
    let head = "the guard started again during the switch dev → live (8 steps done)";
    let g = cut_off(true);
    assert_eq!(g.state.mode, Mode::Live);
    assert_eq!(
        g.state.lifecycle,
        Lifecycle::Prod(Prod {
            since: T0,
            pin: OTHER.into(),
            previous: Some(SHA.into()),
            maintenance: None,
        })
    );
    let note = format!("maintenance build {OTHER} becomes the pin; {SHA} is the previous pin");
    assert!(g.report.contains(&note), "{:?}", g.report);
    assert_eq!(
        texts(&g),
        [format!(
            "{head}; in prod nothing goes to REAPER on its own: its engine was armed, so the PC \
             is in live with it (ide event keeps it as it is): iemmode live --build {OTHER} \
             starts it all again, iemmode event starts REAPER (the PC stays prod)"
        )]
    );
    let g = cut_off(false);
    assert_eq!(g.state.mode, Mode::Event);
    assert_eq!(g.state.lifecycle, prod_on(None, Some(OTHER)));
    assert_eq!(texts(&g), [format!("{head}; {}", no_reaper(OTHER))]);
    // A maintenance entry on a dev build from live, cut off after its engine
    // armed: dev with that engine; the lifecycle stays (whether the entry
    // named a build is not known).
    let (mut pc, mut g) = prod_live();
    g.state.set_active(OTHER);
    g.state.pids.engine = Some(crate::state::Child {
        pid: 4242,
        start_time: 1,
        image: "iem-engine.exe".into(),
    });
    g.state.switching = Some(Switching {
        from: Mode::Live,
        to: Mode::Dev,
        done: vec![
            Step::Precheck,
            Step::EngineStop,
            Step::EngineStart,
            Step::EngineArm,
        ],
        started: T0,
    });
    g.state.written_at = 0;
    assert_eq!(start(&mut pc, &mut g, 0), None);
    assert_eq!(g.state.mode, Mode::Dev);
    assert_eq!(g.state.lifecycle, prod_on(None, None));
    assert_eq!(
        texts(&g),
        [
            "the guard started again during the switch live → dev (4 steps done); in prod \
             nothing goes to REAPER on its own: its engine was armed, so the PC is in dev with \
             it (asking for it again tries again)"
        ]
    );
}

/// The review of PR #56, finding 8: only the boot's last miss named its
/// attempts; any other failure after the first attempt (a fault, the
/// stop of the engine that missed, the preference check before the next
/// start) read as the first attempt's. Every failure of the boot's live
/// entry after its first attempt names the attempt; the first attempt's
/// own failure names none (`the_boot_s_live_entry_retries_only_missed_periods`).
#[test]
fn every_failure_after_the_boot_s_first_attempt_names_it() {
    let failed = |script: &dyn Fn(&mut FakePc)| {
        let (mut pc, mut g) = booted_in_prod();
        script(&mut pc);
        assert_eq!(start(&mut pc, &mut g, BOOT), Some(Outcome::NeedsOwner));
        assert_eq!(g.state.mode, Mode::Event);
        assert!(!pc.called(Call::ReaperStart) && !pc.called(Call::AppStart));
        texts(&g)
    };
    let alarm = |why: &str, attempt: u32| {
        [format!(
            "EngineArm: {why} (attempt {attempt} of 4 after the boot); {}",
            no_reaper(SHA)
        )]
    };
    let faulted = "the engine faulted";
    let fault_at = |misses: Vec<u64>| {
        move |pc: &mut FakePc| {
            pc.fail_after(Call::EngineReady, misses.len(), faulted);
            pc.ready_misses.clone_from(&misses);
        }
    };
    assert_eq!(failed(&fault_at(vec![1])), alarm(faulted, 2));
    assert_eq!(failed(&fault_at(vec![1, 2])), alarm(faulted, 3));
    assert_eq!(failed(&fault_at(vec![1, 2, 1])), alarm(faulted, 4));
    let stop = "no DriverReleased within 10 s";
    let t = failed(&|pc: &mut FakePc| {
        pc.ready_misses = vec![1];
        pc.fail(Call::EngineStop, stop);
    });
    assert_eq!(t, alarm(stop, 2));
    let pref = "3 restores failed";
    let t = failed(&|pc: &mut FakePc| {
        pc.ready_misses = vec![1, 1];
        pc.fail_after(Call::PrefCheck, 2, pref);
    });
    assert_eq!(t, alarm(pref, 3));
}
