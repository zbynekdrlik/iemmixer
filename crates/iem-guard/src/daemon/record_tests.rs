//! The daemon's records in its replies (S7, #10): the engine's pid, the last
//! switch with its steps timed and its in-ear silence, and texts cut to fit
//! a frame. New daemon tests live here, since `daemon.rs` and
//! `daemon/tests.rs` are over their size budget (#36).

use super::tests::{band_up, iemmixer_up};
use super::*;
use crate::pc::fake::{Call, FakePc};
use crate::plan::Facts;
use crate::state::Child;
use crate::switch_log::{LastSwitch, SwitchOutcome};

/// The generation of a guard that began no switch and routed no "ide event".
const INIT: Generation = Generation { epoch: 0, fence: 0 };

/// A synthetic bundle SHA: a dev entry's identity check names the pin.
fn sha() -> String {
    "a".repeat(40)
}

/// The record of the switch that ended last.
fn last(g: &Guard) -> LastSwitch {
    g.state
        .last_switch
        .clone()
        .expect("a record of the last switch")
}

fn steps_of(r: &LastSwitch) -> Vec<Step> {
    r.steps.iter().map(|s| s.step).collect()
}

/// The record's time from its first `a` through the first `b` after it.
fn sum_over(r: &LastSwitch, a: Step, b: Step) -> u64 {
    let steps = steps_of(r);
    let from = steps.iter().position(|s| *s == a).unwrap();
    let to = from + steps[from..].iter().position(|s| *s == b).unwrap();
    r.steps[from..=to].iter().map(|s| s.ms).sum()
}

/// S7 design note §5: a dev entry keeps its record (from, to, the mode it
/// ended in, the outcome, start and end) with every step of its plan timed
/// from the end of the one before, and its in-ear silence from REAPER's
/// save and quit through the engine's arm.
#[test]
fn a_dev_entry_keeps_its_record_with_every_step_timed() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(sha());
    pc.delay(Call::EngineArm, Duration::from_millis(150));
    let t0 = Instant::now();
    assert_eq!(
        run_switch(&mut pc, &mut g, Mode::Event, Mode::Dev),
        Outcome::Done
    );
    let took = t0.elapsed().as_millis();
    let r = last(&g);
    assert_eq!(
        (r.from, r.to, r.ended_in, r.outcome),
        (Mode::Event, Mode::Dev, Mode::Dev, SwitchOutcome::Done)
    );
    assert_eq!(steps_of(&r), plan(Mode::Dev, &band_up()));
    let arm = r.steps.iter().find(|s| s.step == Step::EngineArm).unwrap();
    assert!(arm.ms >= 150, "{arm:?}");
    // The steps add up to the switch, never more: each is timed from the
    // end of the one before.
    let total: u64 = r.steps.iter().map(|s| s.ms).sum();
    assert!(
        u128::from(total) <= took,
        "{total} ms of steps in {took} ms"
    );
    let quiet = sum_over(&r, Step::ReaperSaveQuit, Step::EngineArm);
    assert!(quiet >= 150, "{quiet}");
    assert_eq!(r.silence_ms, Some(quiet));
}

/// The record names when its switch began (`Switching.started`) and when it
/// ended (the clock at its end), in seconds since the epoch.
#[test]
fn a_record_names_when_its_switch_began_and_ended() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    let Clock::Fixed(t) = g.clock.clone() else {
        panic!("the test guard's clock is fixed")
    };
    g.begin(Mode::Event, Mode::Dev, &[], false);
    t.store(1_790_000_042, Ordering::SeqCst);
    g.finish(&mut pc, Outcome::Done, Mode::Dev);
    let r = last(&g);
    assert_eq!((r.started, r.ended), (1_790_000_000, 1_790_000_042));
    assert!(r.steps.is_empty(), "{:?}", r.steps);
}

/// A failed step is timed and kept: at "ide event" a failed `AppHandover`
/// (policy `ContinueAskOwner` since #10) is alarmed, the plan goes on, the
/// record lists it with its time, and the switch ends `needs_owner`.
#[test]
fn a_failed_step_is_timed_and_kept_in_the_record() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    pc.fail(Call::AppAnswers, "the app does not answer");
    pc.delay(Call::AppAnswers, Duration::from_millis(50));
    assert_eq!(
        run_switch(&mut pc, &mut g, Mode::Dev, Mode::Event),
        Outcome::NeedsOwner
    );
    let r = last(&g);
    assert_eq!(steps_of(&r), plan(Mode::Event, &iemmixer_up()));
    let failed = r.steps.iter().find(|s| s.step == Step::AppHandover);
    assert!(failed.is_some_and(|s| s.ms >= 50), "{failed:?}");
    assert_eq!(
        (r.from, r.to, r.ended_in, r.outcome),
        (
            Mode::Dev,
            Mode::Event,
            Mode::Event,
            SwitchOutcome::NeedsOwner
        )
    );
    assert_eq!(
        r.silence_ms,
        Some(sum_over(&r, Step::EngineStop, Step::ReaperHandover))
    );
}

/// A failed engine stop at "ide event" is followed by the health read the
/// runner inserts (never planned): the record lists both, the read timed
/// once it answered. A plan that stopped there never played REAPER: no
/// silence window.
#[test]
fn a_failed_engine_stop_records_its_health_read() {
    for (health, outcome, ended_in) in [
        (Health::Healthy, SwitchOutcome::KeptServing, Mode::Dev),
        (Health::Parked, SwitchOutcome::NeedsOwner, Mode::Event),
    ] {
        let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
        pc.fail(Call::EngineStop, "no DriverReleased within 10 s");
        pc.health(health);
        pc.delay(Call::EngineHealth, Duration::from_millis(50));
        run_switch(&mut pc, &mut g, Mode::Dev, Mode::Event);
        let r = last(&g);
        assert_eq!(
            steps_of(&r),
            [Step::EngineStop, Step::EngineHealth],
            "{health:?}"
        );
        assert!(r.steps[1].ms >= 50, "{health:?}: {:?}", r.steps);
        assert_eq!(
            (r.from, r.to, r.ended_in, r.outcome),
            (Mode::Dev, Mode::Event, ended_in, outcome),
            "{health:?}"
        );
        assert_eq!(r.silence_ms, None, "{health:?}");
    }
}

/// `iemmode status` and every other reply carry the last switch, from the
/// daemon and from the pipe's view alike.
#[test]
fn every_reply_and_the_pipes_view_carry_the_last_switch() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    assert_eq!(g.reply(true, "").last_switch, None, "no switch yet");
    run_switch(&mut pc, &mut g, Mode::Dev, Mode::Event);
    let rec = g.state.last_switch.clone();
    assert!(rec.is_some());
    assert_eq!(g.reply(true, "").last_switch, rec);
    assert_eq!(
        handle(&mut pc, &mut g, Request::Status, INIT).last_switch,
        rec
    );
    match g.shared.route(&Request::Status) {
        Route::Now(r) => assert_eq!(r.last_switch, rec),
        other => panic!("{other:?}"),
    }
    assert_eq!(g.shared.view().last_switch, rec);
}

/// The record is in the guard's state file: a new guard (a hand-over, a
/// restart) reads it and replies with it at once.
#[test]
fn the_record_survives_a_guard_restart() {
    let dir = tempfile::tempdir().unwrap();
    let clock = || Clock::Fixed(Arc::new(AtomicU64::new(1_790_000_000)));
    let mut g = Guard::open(dir.path(), SiteConf::default(), clock());
    let mut pc = FakePc::new(iemmixer_up());
    run_switch(&mut pc, &mut g, Mode::Dev, Mode::Event);
    let rec = g.state.last_switch.clone();
    assert!(rec.is_some());
    let (saved, err) = GuardState::load(&dir.path().join("guard").join(STATE_FILE));
    assert_eq!(err, None);
    assert_eq!(saved.last_switch, rec);
    let back = Guard::open(dir.path(), SiteConf::default(), clock());
    assert_eq!(back.reply(true, "").last_switch, rec);
    assert_eq!(back.shared.view().last_switch, rec);
}

/// A dev entry that fails unwinds to event (`back_to_event`), and the
/// unwind's record spans both (#10 2026-10-07): the entry's steps up to the
/// failed `EngineArm`, then the unwind's, on one clock; it names the entry
/// (`unwound`); its silence runs from the entry's save and quit through the
/// unwind's handover. The switch after it is one of its own again. (The
/// record's start is the entry's: the fixed clock gives both the same here,
/// so `a_preempted_entry_records_its_steps_then_the_unwind` moves it.)
#[test]
fn an_unwound_dev_entry_records_its_steps_then_the_unwind() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(sha());
    pc.fail(Call::EngineArm, "the engine did not arm");
    pc.delay(Call::EngineArm, Duration::from_millis(100));
    pc.delay(Call::EngineStop, Duration::from_millis(100));
    let t0 = Instant::now();
    assert_eq!(
        run_switch(&mut pc, &mut g, Mode::Event, Mode::Dev),
        Outcome::Done
    );
    let took = t0.elapsed().as_millis();
    let r = last(&g);
    assert_eq!(
        (r.from, r.to, r.ended_in, r.outcome, r.unwound),
        (
            Mode::Event,
            Mode::Event,
            Mode::Event,
            SwitchOutcome::Done,
            Some(Mode::Dev)
        )
    );
    assert_eq!(r.started, 1_790_000_000);
    let entry = plan(Mode::Dev, &band_up());
    let arm = entry.iter().position(|s| *s == Step::EngineArm).unwrap();
    let unwind = plan(
        Mode::Event,
        &Facts {
            engine: true,
            ..Facts::default()
        },
    );
    assert_eq!(steps_of(&r), [&entry[..=arm], &unwind[..]].concat());
    // One clock: the steps add up to the entry and its unwind, never more.
    let total: u64 = r.steps.iter().map(|s| s.ms).sum();
    assert!(
        u128::from(total) <= took,
        "{total} ms of steps in {took} ms"
    );
    // The in-ears went quiet with the entry's save and quit and played
    // again after the unwind's handover: both delayed steps lie inside.
    let quiet = sum_over(&r, Step::ReaperSaveQuit, Step::ReaperHandover);
    assert!(quiet >= 200, "{quiet}");
    assert_eq!(r.silence_ms, Some(quiet));
    // The next switch (the event checks) is one of its own: no entry, its
    // own start, its own steps.
    let Clock::Fixed(t) = g.clock.clone() else {
        panic!("the test guard's clock is fixed")
    };
    t.store(1_790_000_100, Ordering::SeqCst);
    let checks = plan(Mode::Event, &pc.facts);
    run_switch(&mut pc, &mut g, Mode::Event, Mode::Event);
    let next = last(&g);
    assert_eq!((next.unwound, next.started), (None, 1_790_000_100));
    assert_eq!(steps_of(&next), checks);
}

/// "Ide event" pre-empts a dev entry, which unwinds through the same path
/// (#10 2026-10-07): the record spans the entry (its steps up to the
/// pre-empted one) and its unwind, and begins at the entry's start, not at
/// the unwind's.
#[test]
fn a_preempted_entry_records_its_steps_then_the_unwind() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    pc.block_until_cancel(Call::AppStop);
    let Clock::Fixed(t) = g.clock.clone() else {
        panic!("the test guard's clock is fixed")
    };
    let c = g.cancel.clone();
    let fired = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        // The unwind begins 30 s after the entry.
        t.store(1_790_000_030, Ordering::SeqCst);
        c.preempt();
    });
    run_switch(&mut pc, &mut g, Mode::Event, Mode::Dev);
    fired.join().unwrap();
    let r = last(&g);
    assert_eq!(
        (r.from, r.to, r.ended_in, r.unwound),
        (Mode::Event, Mode::Event, Mode::Event, Some(Mode::Dev))
    );
    assert_eq!((r.started, r.ended), (1_790_000_000, 1_790_000_030));
    // A pre-empted step changes nothing: the unwind meets the band's system.
    let prefix = [Step::Precheck, Step::AppStop];
    assert_eq!(
        steps_of(&r),
        [&prefix[..], &plan(Mode::Event, &band_up())[..]].concat()
    );
    // The pre-empted step is timed until the pre-emption (~300 ms in).
    assert!(r.steps[1].ms >= 200, "{:?}", r.steps);
    // The app's stop leaves REAPER playing: nothing went quiet.
    assert_eq!(r.silence_ms, None);
}

/// The watch's way back to REAPER after a crash loop (dev → event) is a
/// switch of its own, not an unwind: its record names no entry and begins
/// at its own start.
#[test]
fn a_crash_fallback_record_has_no_unwound_entry() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Dev));
    pc.exited = vec![(Kid::Engine, Some(70)); 3];
    tick(&mut pc, &mut g, Instant::now());
    assert_eq!(g.state.mode, Mode::Event);
    let r = last(&g);
    assert_eq!(
        (r.from, r.to, r.ended_in, r.unwound),
        (Mode::Dev, Mode::Event, Mode::Event, None)
    );
    assert_eq!(r.started, 1_790_000_000);
    assert_eq!(steps_of(&r), plan(Mode::Event, &Facts::default()));
}

/// The soak's "one pid" (S7 design note §4): `Reply.engine` names the engine
/// process the guard started or adopted (`GuardState.pids`), and none while
/// it knows of none.
#[test]
fn the_engine_reply_names_its_pid() {
    let mut pc = FakePc::new(Facts {
        engine: true,
        ..Facts::default()
    });
    let mut g = Guard::for_test(Mode::Dev);
    g.state.pids.engine = Some(Child {
        pid: 4242,
        start_time: 1,
        image: "iem-engine.exe".into(),
    });
    let r = handle(&mut pc, &mut g, Request::Status, INIT);
    assert_eq!(r.engine.map(|e| e.pid), Some(Some(4242)));
    g.state.pids.engine = None;
    let r = handle(&mut pc, &mut g, Request::Status, INIT);
    assert_eq!(
        r.engine.map(|e| e.pid),
        Some(None),
        "an engine seen, no child"
    );
}

/// JSON escapes a C0 control character to six bytes (`\u001f`), so a reply
/// of such alarm texts could pass the frame (S7 Task 3 review, #10): `cut`
/// turns each one into a space wherever alarm texts and reply details are
/// cut. A line break and a tab (two bytes each; alarm texts use line
/// breaks) stay, and so does everything from the space up.
#[test]
fn texts_are_cut_with_control_characters_as_spaces_but_line_breaks_and_tabs() {
    assert_eq!(
        cut("a\nb\tc\u{1f}d\u{20}e\u{0}f\rg\u{7f}h", 64),
        "a\nb\tc d e f g\u{7f}h"
    );
    // The cap counts characters, the same before and after.
    assert_eq!(cut("\u{1}\u{2}\u{3}", 2), "  ");
    let mut g = Guard::for_test(Mode::Event);
    g.raise(None, "line one\nline\u{1b}[31m two\u{7}", false);
    assert_eq!(g.alarms.last().unwrap().text, "line one\nline [31m two ");
    assert_eq!(g.reply(true, "a\u{8}b\tc").detail, "a b\tc");
    assert_eq!(g.shared.view().reply(true, "a\u{8}b\tc").detail, "a b\tc");
}
