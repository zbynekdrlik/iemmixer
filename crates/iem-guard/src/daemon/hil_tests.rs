//! HIL against `FakePc` (`daemon/hil.rs`): the engine in the reply, the
//! engine's HIL flags, the fault injections, the parked engine and the
//! respawn after a fault.

use std::time::{Duration, Instant};

use super::tests::{INIT, SHA, iemmixer_up, texts};
use super::*;
use crate::pc::Kid;
use crate::pc::fake::{Call, FakePc};
use crate::plan::Facts;
use crate::proto::Request;

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
    let r = handle(&mut pc, &mut g, Request::Status, INIT);
    assert_eq!(r.engine, None, "no engine runs");
    pc.facts.engine = true;
    pc.seen.status.missed = 1;
    pc.seen.status.resets = 2;
    let r = handle(&mut pc, &mut g, Request::Status, INIT);
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
    let r = handle(&mut pc, &mut g, Request::Status, INIT);
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
    let r = handle(&mut pc, &mut g, Request::Status, INIT);
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
    let r = handle(&mut pc, &mut g, Request::InjectFault, INIT);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "inject-fault is for dev; the mode is event")
    );
    g.state.mode = Mode::Dev;
    let r = handle(&mut pc, &mut g, Request::InjectFault, INIT);
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
    let r = handle(&mut pc, &mut g, Request::InjectFault, INIT);
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
    let r = handle(&mut pc, &mut g, Request::InjectSeh, INIT);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "inject-seh is for dev; the mode is event")
    );
    g.state.mode = Mode::Dev;
    let r = handle(&mut pc, &mut g, Request::InjectSeh, INIT);
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
    let r = handle(&mut pc, &mut g, Request::InjectSeh, INIT);
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
        let r = handle(&mut pc, &mut g, Request::InjectPark, INIT);
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
    let r = handle(&mut pc, &mut g, Request::InjectPark, INIT);
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
    let r = handle(&mut pc, &mut g, Request::InjectPark, INIT);
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
    let r = handle(&mut pc, &mut g, Request::InjectPark, INIT);
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
    let r = handle(&mut pc, &mut g, Request::Status, INIT);
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
    let r = handle(&mut pc, &mut g, Request::InjectFault, INIT);
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
    let r = handle(&mut pc, &mut g, Request::Status, INIT);
    assert_eq!(r.engine, None, "no engine between the exit and the respawn");
    tick(&mut pc, &mut g, at + Duration::from_secs(1));
    tick(&mut pc, &mut g, at + Duration::from_secs(5));
    assert_eq!(pc.engine_starts, [(false, true)]);
    let r = handle(&mut pc, &mut g, Request::Status, INIT);
    assert_eq!(r.engine, Some(engine_of(1, Some(70))));
    assert_eq!(g.state.mode, Mode::Dev);
    assert!(g.alarms.all().is_empty(), "{:?}", texts(&g));
}
