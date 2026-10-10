//! S8 (#11): what `iemmode event` does in each lifecycle against `FakePc`
//! (design note `docs/superpowers/specs/2026-10-10-s8-cutover-rollback-design.md`
//! §3.3): there is no rollback (the owner's ROZHODNUTÉ on #11, 2026-10-10),
//! so the engineer's button (no signal) is the event plan in every
//! lifecycle and prod stays prod; the owner's "ide event" (`--signal`) in
//! prod keeps the band's system (lane 3), and never cancels a switch to
//! live (lane 5).

use std::sync::Arc;
use std::time::Duration;

use super::tests::{INIT, SHA, T0, ask, band_up, iemmixer_up, prod_on, record};
use super::*;
use crate::bundle::Hil;
use crate::lifecycle::Lifecycle;
use crate::pc::fake::{Call, FakePc};
use crate::plan::Health;
use crate::proto::Request;

fn event(signal: bool) -> Request {
    Request::Event {
        dry_run: false,
        signal,
    }
}

/// Prod live on `SHA`, iemmixer up.
fn prod_live() -> (FakePc, Guard) {
    let pc = FakePc::new(iemmixer_up());
    let mut g = Guard::for_test(Mode::Live);
    g.state.lifecycle = prod_on(None, None);
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Green));
    g.state.set_active(SHA);
    (pc, g)
}

/// The button (plain `iemmode event`) is the event plan in prod as before
/// the cutover, and the PC stays prod (the next boot goes live on the pin);
/// the owner's "ide event" keeps the band's system.
#[test]
fn in_prod_the_button_is_the_event_plan_and_ide_event_keeps_the_band_s_system() {
    let (mut pc, mut g) = prod_live();
    let r = handle(&mut pc, &mut g, event(false), INIT);
    assert!(r.ok, "{r:?}");
    assert!(r.detail.starts_with("event: done"), "{}", r.detail);
    assert_eq!(
        (g.state.mode, g.state.lifecycle.clone()),
        (Mode::Event, prod_on(None, None))
    );
    assert!(pc.called(Call::EngineStop) && pc.called(Call::ReaperStart));
    assert!(!pc.facts.engine && pc.facts.reaper);
    // "ide event" in prod live with the engine playing: nothing switches.
    let (mut pc, mut g) = prod_live();
    pc.health(Health::Healthy);
    let r = handle(&mut pc, &mut g, event(true), INIT);
    assert!(r.ok, "{r:?}");
    let stay = format!(
        "ide event in prod: iemmixer already serves the band live (prod since {T0}: pin {SHA}, \
         previous none); nothing switched"
    );
    assert_eq!(r.detail, stay);
    assert_eq!(pc.mutating_calls(), Vec::<Call>::new());
    assert_eq!(
        (g.state.mode, g.state.lifecycle.clone()),
        (Mode::Live, prod_on(None, None))
    );
    // …with an engine that does not play: REAPER for this event, still prod.
    let (mut pc, mut g) = prod_live();
    pc.health(Health::Dead);
    let r = handle(&mut pc, &mut g, event(true), INIT);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Event);
    assert_eq!(g.state.lifecycle, prod_on(None, None));
    assert!(pc.called(Call::ReaperStart));
    // …in maintenance: live on the pin, the session's build dropped by its
    // rule (here none).
    let (mut pc, mut g) = prod_live();
    g.state.mode = Mode::Dev;
    let r = handle(&mut pc, &mut g, event(true), INIT);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Live);
    assert_eq!(g.state.lifecycle, prod_on(None, None));
    assert!(!pc.called(Call::ReaperStart) && !pc.called(Call::Data));
    // …in maintenance on a pin that may not go live: REAPER serves.
    let (mut pc, mut g) = prod_live();
    g.state.mode = Mode::Dev;
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Red));
    let r = handle(&mut pc, &mut g, event(true), INIT);
    assert_eq!(g.state.mode, Mode::Event, "{r:?}");
    assert!(
        r.detail.starts_with("in prod live runs the pin"),
        "{}",
        r.detail
    );
    assert!(pc.called(Call::ReaperStart));
    assert_eq!(g.shared.view().running, None, "no switch left showing");
    // …in event (a red pin at the boot): REAPER's checks, nothing else.
    let mut pc = FakePc::new(band_up());
    let mut g = Guard::for_test(Mode::Event);
    g.state.lifecycle = prod_on(None, None);
    let r = handle(&mut pc, &mut g, event(true), INIT);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.lifecycle, prod_on(None, None));
    assert!(!pc.called(Call::EngineStart));
    // Before the cutover both are the event plan, as always.
    for signal in [false, true] {
        let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
        let r = handle(&mut pc, &mut g, event(signal), INIT);
        assert!(r.ok, "{r:?}");
        assert_eq!(
            (g.state.mode, g.state.lifecycle.clone()),
            (Mode::Event, Lifecycle::Trial)
        );
    }
}

/// "ide event --dry-run" in maintenance changes nothing, a pin that may not
/// go live included (the real one would bring REAPER).
#[test]
fn a_dry_ide_event_in_maintenance_changes_nothing() {
    for hil in [Hil::Green, Hil::Red] {
        let (mut pc, mut g) = prod_live();
        g.state.mode = Mode::Dev;
        g.state.bundles.insert(SHA.into(), record(SHA, "main", hil));
        let dry = Request::Event {
            dry_run: true,
            signal: true,
        };
        let r = handle(&mut pc, &mut g, dry, INIT);
        assert_eq!(r.ok, hil == Hil::Green, "{r:?}");
        assert_eq!(pc.mutating_calls(), Vec::<Call>::new(), "{hil:?}");
        assert_eq!(g.state.mode, Mode::Dev);
        assert_eq!(g.shared.view().running, None, "{hil:?}");
    }
}

/// "ide event" that leaves a healthy prod live as it is clears the
/// pre-emption the pipe set as it routed it: the next switch (a maintenance
/// entry) is not pre-empted back to REAPER (lane 3's review).
#[test]
fn a_stay_leaves_no_preemption_for_the_next_switch() {
    let (mut pc, mut g) = prod_live();
    pc.health(Health::Healthy);
    let req = event(true);
    let Route::Queue(seen) = g.shared.route(&req) else {
        panic!("not queued")
    };
    assert!(g.cancel.preempted());
    let r = handle(&mut pc, &mut g, req, seen);
    assert!(r.ok && r.detail.contains("nothing switched"), "{r:?}");
    assert!(!g.cancel.preempted());
    let r = ask(&mut pc, &mut g, super::tests::dev());
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Dev);
    assert!(!pc.called(Call::ReaperStart));
}

/// In prod "ide event" routed while a switch to live runs (an in-flight
/// iempc command's own `iemmode event --signal` next to the owner's) waits
/// for it and never pre-empts it: after the cutover live is the band's
/// system, so the entry goes on and its end counts as done (S8 lane 5,
/// finding 1). Before the cutover it pre-empts as before, and live is no
/// event done; the button pre-empts it in prod too (it is the event plan).
#[test]
fn in_prod_ide_event_waits_for_a_switch_to_live() {
    let (_, mut g) = prod_live();
    g.save();
    g.shared.update(|v| v.running = Some(Mode::Live));
    assert!(matches!(g.shared.route(&event(true)), Route::AwaitEnd(_)));
    assert!(
        !g.cancel.preempted(),
        "a switch to live in prod is never pre-empted by ide event"
    );
    g.shared.update(|v| {
        v.running = None;
        v.last = Some(Outcome::Done);
    });
    let r = g.shared.await_end("waited", Duration::ZERO);
    assert!(r.ok && r.mode == Mode::Live, "{r:?}");
    assert!(r.detail.starts_with("waited; live: "), "{}", r.detail);
    g.shared.update(|v| v.running = Some(Mode::Live));
    assert!(matches!(g.shared.route(&event(false)), Route::AwaitEnd(_)));
    assert!(g.cancel.preempted(), "the button pre-empts it");
    let mut trial = Guard::for_test(Mode::Live);
    trial.save();
    trial.shared.update(|v| v.running = Some(Mode::Live));
    assert!(matches!(
        trial.shared.route(&event(true)),
        Route::AwaitEnd(_)
    ));
    assert!(trial.cancel.preempted(), "before the cutover it pre-empts");
    trial.shared.update(|v| {
        v.running = None;
        v.last = Some(Outcome::Done);
    });
    assert!(!trial.shared.await_end("waited", Duration::ZERO).ok);
}

/// The whole case: the first "ide event" in maintenance goes live on the
/// pin, and a second one routed meanwhile, before the entry began its steps
/// or during them, waits for it; REAPER never starts.
#[test]
fn a_second_ide_event_in_prod_never_cancels_the_first_one_s_live_entry() {
    for at in [Call::SetBundle, Call::EngineStart] {
        let (mut pc, mut g) = prod_live();
        g.state.mode = Mode::Dev;
        g.save();
        pc.route_at = Some((at, Arc::clone(&g.shared), event(true)));
        let r = handle(&mut pc, &mut g, event(true), INIT);
        assert!(r.ok, "{at:?}: {r:?}");
        assert_eq!(g.state.mode, Mode::Live, "{at:?}");
        assert!(!pc.called(Call::ReaperStart), "{at:?}: {:?}", pc.calls());
        assert!(
            matches!(pc.routed, Some(Route::AwaitEnd(_))),
            "{at:?}: {:?}",
            pc.routed
        );
        let second = g.shared.await_end("waited", Duration::ZERO);
        assert!(second.ok && second.mode == Mode::Live, "{at:?}: {second:?}");
    }
}

/// In prod a button queued before a switch began runs all the same, the
/// event plan: "ide event"'s switch to live may have cleared its
/// pre-emption as it claimed the view, and live is no end for the button.
#[test]
fn in_prod_a_button_queued_before_a_switch_still_runs() {
    let (mut pc, mut g) = prod_live();
    g.save();
    let Route::Queue(seen) = g.shared.route(&event(false)) else {
        panic!("not queued")
    };
    g.shared.update(|v| v.epoch += 1);
    let r = handle(&mut pc, &mut g, event(false), seen);
    assert!(r.ok && r.detail.starts_with("event: done"), "{r:?}");
    assert_eq!(g.state.mode, Mode::Event);
    assert!(pc.called(Call::ReaperStart));
    assert_eq!(g.state.lifecycle, prod_on(None, None));
}

/// `iemmode rollback` is gone: an older iemmode's request is no request this
/// guard reads.
#[test]
fn an_older_iemmode_s_rollback_is_no_request() {
    assert!(serde_json::from_str::<Request>(r#"{"cmd":"rollback","dry_run":false}"#).is_err());
}
