//! REAPER's crash on quit and the event path (#10, 2026-10-08, the first
//! live trial's way back: REAPER crashed in `reaper_csurf.dll` on quit,
//! Windows Error Reporting held it past the 30 s quit bound, the unwind's
//! plan saw it and planned no start, its handover waited 120 s for a
//! process that no longer ran, and the switch still ended `done` in event
//! without REAPER). The handover now first makes sure a REAPER runs, and an
//! event switch whose REAPER or app handover failed ends `needs_owner`, its
//! reply naming each failed step (`daemon/reaper.rs`, `daemon/runner.rs`).

use super::tests::{band_up, iemmixer_up};
use super::*;
use crate::pc::fake::{Call, FakePc};
use crate::plan::{Facts, Health, plan};
use crate::proto::Request;
use crate::switch_log::{LastSwitch, SwitchOutcome};

/// A synthetic bundle SHA: a dev entry's identity check names the pin.
fn sha() -> String {
    "a".repeat(40)
}

fn texts(g: &Guard) -> Vec<String> {
    g.alarms.iter().map(|a| a.text.clone()).collect()
}

/// The calls that act or wait, without the reads every step and switch
/// makes (facts, children, the process list, the drift, the notices).
fn acts(calls: &[Call]) -> Vec<Call> {
    calls
        .iter()
        .copied()
        .filter(|c| {
            !matches!(
                c,
                Call::Facts | Call::Children | Call::Procs | Call::TuningDrift | Call::Notify
            )
        })
        .collect()
}

fn last(g: &Guard) -> LastSwitch {
    g.state
        .last_switch
        .clone()
        .expect("a record of the last switch")
}

fn steps_of(r: &LastSwitch) -> Vec<Step> {
    r.steps.iter().map(|s| s.step).collect()
}

/// What the unwind of the 2026-10-08 trial does after the failed quit: its
/// plan has no start (REAPER ran when it read its facts), its handover
/// waits for the crashing REAPER to be gone, checks the preference, starts
/// REAPER and then checks it; the app follows.
const UNWIND_AFTER_THE_CRASH: [Call; 9] = [
    Call::Tuning,
    Call::PrefCheck,
    Call::ReaperAwaitEnd,
    Call::PrefCheck,
    Call::ReaperStart,
    Call::ReaperFacts,
    Call::AppStart,
    Call::AppAnswers,
    Call::Fingerprint,
];

/// The trial's failure (#10, 2026-10-08): the quit failed after a verified
/// save, REAPER still runs and Windows Error Reporting holds it. The unwind
/// ends `done` in event with REAPER running, started inside the handover's
/// time (no step of its own in the record).
#[test]
fn an_unwind_waits_for_a_reaper_crashing_on_quit_and_starts_it() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(sha());
    let why = "REAPER crashed on quit and Windows Error Reporting still holds it after 30 + 90 s";
    pc.fail(Call::ReaperSaveQuit, why);
    pc.reaper_ending = true;
    assert_eq!(
        run_switch(&mut pc, &mut g, Mode::Event, Mode::Dev),
        Outcome::Done
    );
    assert_eq!(g.state.mode, Mode::Event);
    assert_eq!(
        acts(&pc.calls_after(Call::ReaperSaveQuit)),
        UNWIND_AFTER_THE_CRASH
    );
    assert!(pc.facts.reaper && pc.facts.reaper_holds_module);
    let r = last(&g);
    assert_eq!(
        (r.outcome, r.ended_in, r.unwound),
        (SwitchOutcome::Done, Mode::Event, Some(Mode::Dev))
    );
    let steps = steps_of(&r);
    assert!(!steps.contains(&Step::ReaperStart), "{steps:?}");
    assert!(steps.contains(&Step::ReaperHandover), "{steps:?}");
    assert_eq!(texts(&g), [format!("ReaperSaveQuit: {why}")]);
    assert!(g.alarms.iter().all(|a| !a.owner_question));
}

/// "Ide event" during the quit's wait ("ide event" pre-empts the crash
/// hold): the entry unwinds at once, and the unwind's handover waits for
/// the crashing REAPER and starts it, as above.
#[test]
fn ide_event_during_the_crash_hold_unwinds_and_the_handover_starts_reaper() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(sha());
    pc.block_until_cancel(Call::ReaperSaveQuit);
    pc.reaper_ending = true;
    let c = g.cancel.clone();
    let fired = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        c.preempt();
    });
    let t = Instant::now();
    assert_eq!(
        run_switch(&mut pc, &mut g, Mode::Event, Mode::Dev),
        Outcome::Done
    );
    fired.join().unwrap();
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    assert_eq!(g.state.mode, Mode::Event);
    assert_eq!(
        acts(&pc.calls_after(Call::ReaperSaveQuit)),
        UNWIND_AFTER_THE_CRASH
    );
    assert!(pc.facts.reaper);
    assert_eq!(last(&g).unwound, Some(Mode::Dev));
    assert!(g.alarms.all().is_empty(), "{:?}", texts(&g));
}

/// A REAPER that ended by itself after the plan read its facts (no Windows
/// Error Reporting, nothing ending): "ide event" starts it inside the
/// handover, the preference checked right before, and ends `done`.
#[test]
fn a_reaper_that_ended_after_the_plan_is_started_inside_the_handover() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    pc.reaper_ends_at = Some(Call::PrefCheck);
    let seen = g.shared.generation();
    let r = handle(
        &mut pc,
        &mut g,
        Request::Event {
            dry_run: false,
            signal: false,
        },
        seen,
    );
    assert!(r.ok, "{r:?}");
    assert!(
        r.detail.starts_with("event: done; tuning exit: exit: ok; "),
        "{}",
        r.detail
    );
    assert!(
        r.detail
            .contains("REAPER does not run: the handover starts it"),
        "{}",
        r.detail
    );
    assert_eq!(
        acts(&pc.calls()),
        [
            Call::Tuning,
            Call::PrefCheck,
            Call::PrefCheck,
            Call::ReaperStart,
            Call::ReaperFacts,
            Call::AppAnswers,
            Call::Fingerprint,
        ]
    );
    assert!(pc.facts.reaper && pc.facts.reaper_holds_module);
    let rec = last(&g);
    assert_eq!(steps_of(&rec), plan(Mode::Event, &band_up()));
    assert_eq!(rec.outcome, SwitchOutcome::Done);
    assert!(g.alarms.all().is_empty(), "{:?}", texts(&g));
}

/// Never a second REAPER: a start whose process does not show yet (the
/// plan's `ReaperStart`, or the handover's own) is followed by the checks,
/// never by another start (its load poll waits for it).
#[test]
fn a_reaper_whose_process_shows_late_is_never_started_twice() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    pc.reaper_shows_late = true;
    assert_eq!(
        run_switch(&mut pc, &mut g, Mode::Dev, Mode::Event),
        Outcome::Done
    );
    assert_eq!(pc.count(Call::ReaperStart), 1);
    assert_eq!(pc.count(Call::PrefCheck), 1);
    assert!(pc.index(Call::ReaperStart) < pc.index(Call::ReaperFacts));
    assert!(!pc.called(Call::ReaperAwaitEnd));

    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    pc.reaper_ends_at = Some(Call::PrefCheck);
    pc.reaper_shows_late = true;
    assert_eq!(
        run_switch(&mut pc, &mut g, Mode::Event, Mode::Event),
        Outcome::Done
    );
    assert_eq!(pc.count(Call::ReaperStart), 1);
    assert_eq!(pc.count(Call::PrefCheck), 2);
}

/// A REAPER that is still ending after the handover's wait is never started
/// next to and never checked: the handover fails, the owner's question,
/// and the switch ends `needs_owner` in event (the app still starts).
#[test]
fn a_reaper_still_ending_after_the_hold_ends_the_switch_needs_owner() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    pc.reaper_ending = true;
    pc.reaper_held = true;
    assert_eq!(
        run_switch(&mut pc, &mut g, Mode::Event, Mode::Event),
        Outcome::NeedsOwner
    );
    assert_eq!(g.state.mode, Mode::Event);
    assert_eq!(pc.count(Call::ReaperAwaitEnd), 1);
    assert!(!pc.called(Call::ReaperStart) && !pc.called(Call::ReaperFacts));
    assert!(pc.called(Call::AppAnswers) && pc.called(Call::Fingerprint));
    let a = g.alarms.last().unwrap();
    assert_eq!(a.step, Some(Step::ReaperHandover));
    assert!(a.owner_question);
    assert!(
        a.text.starts_with("ReaperHandover: REAPER is still ending"),
        "{}",
        a.text
    );
    let r = last(&g);
    assert_eq!(
        (r.outcome, r.ended_in),
        (SwitchOutcome::NeedsOwner, Mode::Event)
    );

    // A start the start path refuses (I3: an engine or another holder of
    // the driver module) is no done either.
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    pc.reaper_ends_at = Some(Call::PrefCheck);
    pc.fail(
        Call::ReaperStart,
        "an engine runs: REAPER may not start (I3)",
    );
    assert_eq!(
        run_switch(&mut pc, &mut g, Mode::Event, Mode::Event),
        Outcome::NeedsOwner
    );
    assert_eq!(
        texts(&g),
        ["ReaperHandover: an engine runs: REAPER may not start (I3)"]
    );
    assert!(g.alarms.last().unwrap().owner_question);
    assert!(!pc.called(Call::ReaperFacts));
}

/// An unreadable process list is never "no REAPER" (review of the lane):
/// the handover starts nothing, checks nothing, and the switch ends
/// `needs_owner` with the owner's question.
#[test]
fn an_unreadable_process_list_starts_no_reaper() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    pc.reaper_procs_fail = Some("the process list: access denied".into());
    assert_eq!(
        run_switch(&mut pc, &mut g, Mode::Event, Mode::Event),
        Outcome::NeedsOwner
    );
    assert!(!pc.called(Call::ReaperStart) && !pc.called(Call::ReaperFacts));
    assert_eq!(
        texts(&g),
        ["ReaperHandover: the process list: access denied"]
    );
    assert!(g.alarms.last().unwrap().owner_question);
}

/// #10's last line ("switch ended in event: done" without REAPER): a
/// handover that fails ends the switch `needs_owner`, its alarm names the
/// step and is the owner's question, and `iemmode event` gets `ok: false`
/// (exit 1) from the daemon and from the view alike.
#[test]
fn a_failed_reaper_handover_is_never_done() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    pc.fail(Call::ReaperFacts, "REAPER does not run");
    let seen = g.shared.generation();
    let r = handle(
        &mut pc,
        &mut g,
        Request::Event {
            dry_run: false,
            signal: false,
        },
        seen,
    );
    assert!(!r.ok, "{r:?}");
    assert_eq!(crate::cli::exit_code(&r), 1);
    assert!(
        r.detail.starts_with(
            "event: ended, needs the owner: ReaperHandover failed: REAPER does not run"
        ),
        "{}",
        r.detail
    );
    assert_eq!(r.mode, Mode::Event);
    let a = g.alarms.last().unwrap();
    assert_eq!(
        (a.step, a.text.as_str(), a.owner_question),
        (
            Some(Step::ReaperHandover),
            "ReaperHandover: REAPER does not run",
            true
        )
    );
    // The plan went on to the app (the band keeps what works).
    assert!(pc.called(Call::AppStart) && pc.called(Call::AppAnswers));
    assert!(pc.called(Call::Fingerprint));
    let rec = last(&g);
    assert_eq!(
        (rec.outcome, rec.ended_in),
        (SwitchOutcome::NeedsOwner, Mode::Event)
    );
    let routed = g.shared.view().event_reply("routed");
    assert!(!routed.ok);
    assert!(
        routed.detail.starts_with(
            "routed; event: ended, needs the owner: ReaperHandover failed: REAPER does not run"
        ),
        "{}",
        routed.detail
    );
}

/// The coordinator's decision on #10 (2026-10-08): an event switch that
/// ends without the predecessor app serving is not done. REAPER keeps
/// playing the band's mixes, but the phones cannot change them, so a
/// failed app handover asks the owner; the plan goes on (the fingerprint).
#[test]
fn a_failed_app_handover_ends_the_switch_needs_owner_with_reaper_running() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    pc.fail(Call::AppAnswers, "the app does not answer on port 80");
    let seen = g.shared.generation();
    let r = handle(
        &mut pc,
        &mut g,
        Request::Event {
            dry_run: false,
            signal: false,
        },
        seen,
    );
    assert!(!r.ok, "{r:?}");
    assert!(
        r.detail.starts_with(
            "event: ended, needs the owner: AppHandover failed: the app does not answer on \
             port 80"
        ),
        "{}",
        r.detail
    );
    assert_eq!(r.mode, Mode::Event);
    assert!(pc.facts.reaper && pc.facts.reaper_holds_module);
    assert!(pc.called(Call::Fingerprint));
    // The plan ran to its end: the tuning drift is read as after any such
    // switch.
    assert!(pc.called(Call::TuningDrift));
    assert_eq!(
        texts(&g),
        ["AppHandover: the app does not answer on port 80"]
    );
    let a = g.alarms.last().unwrap();
    assert_eq!(a.step, Some(Step::AppHandover));
    assert!(a.owner_question);
    let rec = last(&g);
    assert_eq!(
        (rec.outcome, rec.ended_in),
        (SwitchOutcome::NeedsOwner, Mode::Event)
    );
}

/// A failed app stop leaves no app serving (the event plan stops only an
/// app that does not serve, and skips the start after the failure): the
/// switch ends `needs_owner`, the rest of the plan still runs.
#[test]
fn a_failed_app_stop_ends_the_switch_needs_owner() {
    let (mut pc, mut g) = (
        FakePc::new(Facts {
            app_serves: false,
            ..band_up()
        }),
        Guard::for_test(Mode::Event),
    );
    pc.app_exit.exit_code = None;
    let seen = g.shared.generation();
    let r = handle(
        &mut pc,
        &mut g,
        Request::Event {
            dry_run: false,
            signal: false,
        },
        seen,
    );
    assert!(!r.ok, "{r:?}");
    assert!(
        r.detail.starts_with(
            "event: ended, needs the owner: AppStop failed: the app did not exit within 30 s"
        ),
        "{}",
        r.detail
    );
    assert!(!pc.called(Call::AppStart));
    assert!(pc.called(Call::AppAnswers) && pc.called(Call::Fingerprint));
    let a = g.alarms.last().unwrap();
    assert_eq!(a.step, Some(Step::AppStop));
    assert!(a.owner_question);
}

/// A plan that stopped for the owner keeps its words, in the routed "ide
/// event" reply too: no step went on after asking.
#[test]
fn a_stopped_plan_keeps_its_words_in_the_routed_reply() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    pc.fail(Call::EngineStop, "no DriverReleased within 10 s");
    pc.health(Health::Parked);
    assert_eq!(
        run_switch(&mut pc, &mut g, Mode::Dev, Mode::Event),
        Outcome::NeedsOwner
    );
    let routed = g.shared.view().event_reply("routed");
    assert!(!routed.ok);
    assert_eq!(routed.detail, "routed; event: stopped; the owner decides");
}

/// Every step that asked the owner while the plan went on is named, in
/// order; an entry whose unwind needed the owner says it was not entered.
#[test]
fn the_reply_names_every_step_that_asked_the_owner() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    pc.reaper.heartbeat_advanced = false;
    pc.fail(Call::AppAnswers, "the app does not answer");
    let seen = g.shared.generation();
    let r = handle(
        &mut pc,
        &mut g,
        Request::Event {
            dry_run: false,
            signal: false,
        },
        seen,
    );
    assert!(
        r.detail.starts_with(
            "event: ended, needs the owner: ReaperHandover failed: the meter heartbeat does \
             not advance; AppHandover failed: the app does not answer"
        ),
        "{}",
        r.detail
    );

    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(sha());
    pc.fail(Call::ReaperSaveQuit, "REAPER did not quit within 30 s");
    pc.fail(Call::ReaperFacts, "REAPER does not run");
    let seen = g.shared.generation();
    let r = handle(
        &mut pc,
        &mut g,
        Request::Dev {
            build: None,
            dry_run: false,
        },
        seen,
    );
    assert!(!r.ok);
    assert!(
        r.detail.starts_with(
            "dev: not entered; event: ended, needs the owner: ReaperHandover failed: REAPER \
             does not run"
        ),
        "{}",
        r.detail
    );
    // The next switch that needs nobody says done again.
    let mut pc = FakePc::new(band_up());
    let seen = g.shared.generation();
    let r = handle(
        &mut pc,
        &mut g,
        Request::Event {
            dry_run: false,
            signal: false,
        },
        seen,
    );
    assert!(r.ok, "{r:?}");
    assert!(r.detail.starts_with("event: done"), "{}", r.detail);
    assert!(g.shared.view().event_reply("routed").ok);
}

/// I2 inside the handover: the plan's check met the crashing REAPER on the
/// card and wrote nothing (one alarm); once it was gone, the handover's
/// check restored REAPER's original right before its start.
#[test]
fn the_handovers_start_restores_the_preference_first() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    pc.pref_attempts = 1;
    pc.reaper_ending = true;
    assert_eq!(
        run_switch(&mut pc, &mut g, Mode::Event, Mode::Event),
        Outcome::Done
    );
    assert_eq!(pc.pref_writes, 1);
    assert_eq!(
        acts(&pc.calls()),
        [
            Call::Tuning,
            Call::PrefCheck,
            Call::ReaperAwaitEnd,
            Call::PrefCheck,
            Call::ReaperStart,
            Call::ReaperFacts,
            Call::AppAnswers,
            Call::Fingerprint,
        ]
    );
    assert_eq!(g.state.pref_held, None);
    let t = texts(&g);
    assert_eq!(t.len(), 1, "{t:?}");
    assert!(t[0].starts_with("PrefCheck: "), "{t:?}");
}

/// A check that cannot read the preference before the handover's start
/// follows `[guard] on_pref_fail` as the plan's own check does:
/// `start_reaper_with_alarm` alarms and starts REAPER.
#[test]
fn the_handovers_start_follows_on_pref_fail() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.site.on_pref_fail = PrefFail::StartReaperWithAlarm;
    pc.reaper_ends_at = Some(Call::Tuning);
    pc.fail(Call::PrefCheck, "the registry is locked");
    assert_eq!(
        run_switch(&mut pc, &mut g, Mode::Event, Mode::Event),
        Outcome::Done
    );
    assert_eq!(pc.count(Call::ReaperStart), 1);
    assert!(pc.facts.reaper);
    assert_eq!(
        texts(&g),
        [
            "PrefCheck: the registry is locked",
            "PrefCheck: the registry is locked (before the handover's REAPER start)",
        ]
    );
    assert!(g.alarms.iter().all(|a| !a.owner_question));
}
