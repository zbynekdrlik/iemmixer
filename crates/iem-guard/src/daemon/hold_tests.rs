//! In prod nothing goes to REAPER on its own (the owner's decision on #11,
//! 2026-10-19): what a dev or live entry that stops leaves, against
//! `FakePc` (the review of PR #56). An entry that stops after a step changed
//! what runs ends in event, where the crash watch respawns nothing and both
//! ways on run from; one that changed nothing stays in the mode it began
//! from. The same rule holds for an entry a restarted guard finds cut off.
//! The entry's build becomes the active bundle only once its engine starts.

use std::time::{Duration, Instant};

use super::tests::{INIT, OTHER, SHA, T0, ask, iemmixer_up, no_reaper, prod_on, record, texts};
use super::*;
use crate::bundle::{Hil, Pins};
use crate::pc::Kid;
use crate::pc::fake::{Call, FakePc};
use crate::plan::Health;
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
    // "ide event": live on the pin, never a stay on the held engine, whose
    // callbacks advance.
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
/// in prod (the review of PR #56, finding 1): once a step it finished
/// changed what runs it stops in event (a HIL job, which lives in dev only,
/// ends with dev); while only its precheck finished it stays in the mode it
/// began from. Nothing starts and nothing stops either way.
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
        let g = left(from, to, vec![Step::Precheck, Step::EngineStop]);
        assert_eq!(g.state.mode, Mode::Event, "{from:?} → {to:?}");
        assert_eq!(g.shared.view().mode, Mode::Event, "{from:?} → {to:?}");
        assert_eq!(g.state.job, None, "{from:?} → {to:?}");
        for done in [vec![Step::Precheck], Vec::new()] {
            let g = left(from, to, done.clone());
            assert_eq!(g.state.mode, from, "{from:?} → {to:?} {done:?}");
            assert_eq!(
                g.state.job,
                (from == Mode::Dev).then_some(7),
                "{from:?} → {to:?} {done:?}"
            );
        }
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
/// stopped before it changed anything (its precheck, or the old engine's
/// stop) left the pin's engine playing in live with another build active:
/// that engine's next crash respawned the other build unheld in live (G8),
/// and the status and the identity check named it. The entry's build
/// becomes the active bundle once its engine starts; until then the active
/// bundle, its way back and the legacy pins stay as they were, and `pc`
/// runs the active bundle again.
#[test]
fn an_entry_s_build_becomes_the_active_bundle_only_once_its_engine_starts() {
    for (call, why) in [
        (Call::Precheck, "no PWA notification subscription"),
        (Call::EngineStop, "no DriverReleased within 10 s"),
    ] {
        let (mut pc, mut g) = prod_live();
        let before = actives(&g);
        pc.fail(call, why);
        let r = handle(&mut pc, &mut g, dev_other(), INIT);
        assert!(!r.ok, "{call:?}: {r:?}");
        assert_eq!(g.state.mode, Mode::Live, "{call:?}");
        assert_eq!(actives(&g), before, "{call:?}");
        assert_eq!(pc.bundle.as_deref(), Some(SHA), "{call:?}");
        let status = status_text(&g);
        assert!(
            status.contains(&format!("bundle {SHA}")),
            "{call:?}: {status}"
        );
        // The pin's engine ends: the pin's engine again, unheld.
        pc.exited = vec![(Kid::Engine, Some(70))];
        pc.facts.engine = false;
        let at = Instant::now();
        tick(&mut pc, &mut g, at);
        tick(&mut pc, &mut g, at + Duration::from_secs(20));
        assert_eq!(pc.engine_starts, [(false, false)], "{call:?}");
        assert_eq!(pc.bundle.as_deref(), Some(SHA), "{call:?}");
    }
    // Stopped after the old engine's stop, before its own engine started:
    // nothing runs, and the active bundle stays.
    let (mut pc, mut g) = prod_live();
    let before = actives(&g);
    pc.fail(Call::PrefCheck, "3 restores failed");
    let r = handle(&mut pc, &mut g, dev_other(), INIT);
    assert!(!r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Event);
    assert!(pc.engine_starts.is_empty());
    assert_eq!(actives(&g), before);
    assert_eq!(pc.bundle.as_deref(), Some(SHA));
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
