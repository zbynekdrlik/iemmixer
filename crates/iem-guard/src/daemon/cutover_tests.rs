//! S8 lane 2 (#11): the cutover against `FakePc` (design note
//! `docs/superpowers/specs/2026-10-10-s8-cutover-rollback-design.md` §3.2):
//! the steps in their order with their read-backs, the refusals and the dry
//! run, the unwind to trial and event from every step (a failure, an undo
//! that fails, "ide event"), and a cutover a restart cut off.

use super::tests::{INIT, SHA, T0, ask, fixed, iemmixer_up, record, texts};
use super::*;
use crate::bundle::Hil;
use crate::cutover::{CutStep, Run, export_name, plan_text};
use crate::lifecycle::{Lifecycle, Prod};
use crate::pc::fake::{Call, FakePc};
use crate::plan::Health;
use crate::proto::Request;

/// The server config as the cutover leaves it, and as before.
const OPEN: &str = "port = 80\npin_changes = true\n";
const FROZEN: &str = "port = 80\npin_changes = false\n";

fn cut(dry_run: bool) -> Request {
    Request::Cutover {
        build: SHA.into(),
        dry_run,
    }
}

/// Prod as the cutover at `T0` leaves it.
fn prod() -> Lifecycle {
    Lifecycle::Prod(Prod {
        since: T0,
        pin: SHA.into(),
        previous: None,
        maintenance: None,
    })
}

/// A PC the cutover may run on: trial, dev on `SHA` (installed green from
/// main), iemmixer up, the engine healthy at 32.
fn ready() -> (FakePc, Guard) {
    let mut pc = FakePc::new(iemmixer_up());
    pc.health(Health::Healthy);
    let mut g = Guard::for_test(Mode::Dev);
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Green));
    g.state.set_active(SHA);
    (pc, g)
}

#[test]
fn a_cutover_runs_its_steps_in_order_and_ends_in_prod() {
    let (mut pc, mut g) = ready();
    let r = handle(&mut pc, &mut g, cut(false), INIT);
    assert!(r.ok, "{r:?}");
    let export = export_name(T0);
    let done = format!(
        "cutover done: prod since {T0} on the pin {SHA}; the predecessor's autostarts are \
         saved in {export}"
    );
    assert!(r.detail.starts_with(&done), "{}", r.detail);
    assert_eq!(g.state.lifecycle, prod());
    assert_eq!(g.state.cutover, None);
    assert_eq!(g.state.mode, Mode::Live);
    assert_eq!(pc.bundle.as_deref(), Some(SHA));
    // The final import is the live trial's data refresh, before anything else.
    assert!(pc.index(Call::Data) < pc.index(Call::AutostartsOff));
    assert_eq!(
        pc.calls_after(Call::AutostartsOff),
        [
            Call::GuardLogon,
            Call::ServerConfig,
            Call::WriteServerConfig,
            Call::ServerConfig,
            Call::Facts,
            Call::ServerStop,
            Call::ServerStart,
            Call::Children,
            Call::Identity,
            Call::MemberPage,
            Call::EngineHealth,
        ]
    );
    assert_eq!(pc.autostarts_in.as_deref(), Some(export.as_str()));
    assert!(pc.guard_at_logon);
    assert_eq!(pc.server_config, OPEN);
    assert_eq!(texts(&g), Vec::<String>::new());
}

/// Saved and read back: a new guard finds prod on the pin and no cutover
/// in progress.
#[test]
fn the_cutover_is_saved_for_the_next_guard() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    g.state.mode = Mode::Dev;
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Green));
    g.state.set_active(SHA);
    let mut pc = FakePc::new(iemmixer_up());
    pc.health(Health::Healthy);
    let r = handle(&mut pc, &mut g, cut(false), INIT);
    assert!(r.ok, "{r:?}");
    let back = Guard::open(dir.path(), SiteConf::default(), fixed(T0 + 5));
    assert_eq!(back.state.lifecycle, prod());
    assert_eq!(back.state.cutover, None);
    assert_eq!(back.state.mode, Mode::Live);
}

/// In prod the server starts with `pin_changes = true` (a later live entry
/// on the pin); before the cutover the same config refuses it.
#[test]
fn after_the_cutover_the_server_starts_with_pin_changes_on() {
    let (mut pc, mut g) = ready();
    assert!(handle(&mut pc, &mut g, cut(false), INIT).ok);
    let live = Request::Live {
        build: None,
        trial: false,
        dry_run: false,
    };
    let r = ask(&mut pc, &mut g, live);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Live);
    let (mut pc, mut g) = ready();
    pc.server_config = OPEN.into();
    let trial = Request::Live {
        build: Some(SHA.into()),
        trial: true,
        dry_run: false,
    };
    let r = handle(&mut pc, &mut g, trial, INIT);
    assert!(!r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Event);
    assert!(
        texts(&g)
            .iter()
            .any(|t| t.contains("the server config does not set pin_changes = false")),
        "{:?}",
        texts(&g)
    );
}

#[test]
fn a_refused_cutover_changes_nothing() {
    let (mut pc, mut g) = ready();
    g.state.mode = Mode::Event;
    let r = handle(&mut pc, &mut g, cut(false), INIT);
    assert_eq!(
        (r.ok, r.detail),
        (
            false,
            format!("the guard is in event: the cutover runs from dev or a live trial on {SHA}")
        )
    );
    let (mut pc2, mut g2) = ready();
    g2.state.lifecycle = prod();
    let r = handle(&mut pc2, &mut g2, cut(false), INIT);
    assert_eq!(
        (r.ok, r.detail),
        (
            false,
            format!("the cutover is done: prod since {T0} on the pin {SHA}")
        )
    );
    for (pc, g) in [(&pc, &g), (&pc2, &g2)] {
        assert_eq!(pc.calls(), Vec::<Call>::new());
        assert_eq!(g.state.cutover, None);
    }
    assert_eq!(g.state.lifecycle, Lifecycle::Trial);
}

#[test]
fn the_dry_run_names_the_steps_and_the_trial_s_precheck_and_changes_nothing() {
    let (mut pc, mut g) = ready();
    let r = handle(&mut pc, &mut g, cut(true), INIT);
    assert_eq!(
        (r.ok, r.detail),
        (
            true,
            format!(
                "dry run: cutover of {SHA}: {}; precheck ok",
                plan_text(SHA, T0)
            )
        )
    );
    assert_eq!(pc.calls(), [Call::Precheck]);
    assert_eq!(
        (
            g.state.lifecycle.clone(),
            g.state.cutover.clone(),
            g.state.mode
        ),
        (Lifecycle::Trial, None, Mode::Dev)
    );
    // The live trial's precheck refuses the dry run as it would the import.
    pc.subscriptions = Some(0);
    let r = ask(&mut pc, &mut g, cut(true));
    assert!(!r.ok, "{r:?}");
    assert!(
        r.detail.starts_with(&format!(
            "dry run: cutover of {SHA}: {}; precheck ",
            plan_text(SHA, T0)
        )),
        "{}",
        r.detail
    );
    assert!(!r.detail.ends_with("precheck ok"), "{}", r.detail);
}

/// Every step's failure unwinds: the event plan first (the band back on
/// REAPER), then what the begun steps changed, newest first; the lifecycle
/// stays trial and the alarm names the step.
#[test]
fn a_failed_step_unwinds_to_trial_and_event() {
    for (call, step, undo) in [
        (Call::AutostartsOff, "Autostarts", vec![Call::AutostartsOn]),
        (
            Call::WriteServerConfig,
            "PinChanges",
            vec![
                Call::ServerConfig,
                Call::ServerConfig,
                Call::GuardLogon,
                Call::AutostartsOn,
            ],
        ),
        (
            Call::MemberPage,
            "Checks",
            vec![
                Call::ServerConfig,
                Call::WriteServerConfig,
                Call::ServerConfig,
                Call::GuardLogon,
                Call::AutostartsOn,
            ],
        ),
    ] {
        let (mut pc, mut g) = ready();
        pc.fail(call, "refused");
        let r = handle(&mut pc, &mut g, cut(false), INIT);
        let head =
            format!("cutover of {SHA} failed at {step}: refused; unwound to trial and event");
        assert!(!r.ok, "{call:?}");
        assert!(r.detail.starts_with(&head), "{call:?}: {}", r.detail);
        assert_eq!(texts(&g).last(), Some(&head), "{call:?}");
        assert!(g.alarms.iter().all(|a| !a.owner_question), "{call:?}");
        assert_eq!(g.state.lifecycle, Lifecycle::Trial, "{call:?}");
        assert_eq!(g.state.cutover, None, "{call:?}");
        assert_eq!(g.state.mode, Mode::Event, "{call:?}");
        assert!(pc.called(Call::ReaperStart), "{call:?}");
        assert_eq!(
            (
                pc.guard_at_logon,
                pc.autostarts_in.clone(),
                pc.server_config.as_str()
            ),
            (false, None, FROZEN),
            "{call:?}"
        );
        // The undo follows the event plan (its last step, then its drift
        // read), then the alarm's notice.
        let mut after = vec![Call::TuningDrift];
        after.extend(undo);
        after.push(Call::Notify);
        assert_eq!(pc.calls_after(Call::Fingerprint), after, "{call:?}");
    }
}

/// A failed import is the live entry's own unwind (it already went back to
/// event); nothing else began, so nothing more changes.
#[test]
fn a_failed_import_ends_in_event_with_nothing_else_begun() {
    let (mut pc, mut g) = ready();
    pc.fail(Call::Data, "the import refused");
    let r = handle(&mut pc, &mut g, cut(false), INIT);
    assert!(!r.ok);
    let head = format!("cutover of {SHA} failed at Import: the live entry: ");
    assert!(r.detail.starts_with(&head), "{}", r.detail);
    assert!(
        r.detail.contains("; unwound to trial and event"),
        "{}",
        r.detail
    );
    assert_eq!(g.state.mode, Mode::Event);
    assert_eq!(pc.count(Call::ReaperStart), 1, "one event plan");
    for call in [
        Call::AutostartsOff,
        Call::AutostartsOn,
        Call::GuardLogon,
        Call::ServerConfig,
    ] {
        assert!(!pc.called(call), "{call:?}");
    }
    assert_eq!(g.state.cutover, None);
}

/// The checks read the engine: at another period the cutover unwinds.
#[test]
fn an_engine_not_at_32_fails_the_checks() {
    let (mut pc, mut g) = ready();
    pc.seen.status.frames = 64;
    let r = handle(&mut pc, &mut g, cut(false), INIT);
    assert!(
        r.detail.starts_with(&format!(
            "cutover of {SHA} failed at Checks: the engine runs at 64 frames per period, not 32"
        )),
        "{}",
        r.detail
    );
    assert_eq!(g.state.lifecycle, Lifecycle::Trial);
}

/// A config write that does not land is caught by its read-back.
#[test]
fn a_config_that_does_not_read_back_fails_its_step() {
    let (mut pc, mut g) = ready();
    pc.config_sticks = true;
    let r = handle(&mut pc, &mut g, cut(false), INIT);
    assert!(
        r.detail.starts_with(&format!(
            "cutover of {SHA} failed at PinChanges: the server config's pin_changes reads back \
             as Some(false), not true"
        )),
        "{}",
        r.detail
    );
    assert_eq!(pc.server_config, FROZEN);
    assert!(!pc.guard_at_logon);
}

/// "ide event" during a step: the step finishes (a mutation), the next one
/// never begins, and the cutover unwinds.
#[test]
fn ide_event_during_a_step_unwinds_after_it() {
    let (mut pc, mut g) = ready();
    pc.preempt_at = Some((Call::GuardLogon, g.cancel.clone()));
    let r = handle(&mut pc, &mut g, cut(false), INIT);
    let head = format!(
        "cutover of {SHA} failed at PinChanges: pre-empted by event; unwound to trial and event"
    );
    assert!(r.detail.starts_with(&head), "{}", r.detail);
    assert!(!pc.called(Call::ServerConfig), "PinChanges never began");
    assert_eq!(pc.count(Call::GuardLogon), 2, "on, then off");
    assert!(!pc.guard_at_logon);
    assert_eq!(pc.autostarts_in, None);
    assert_eq!(g.state.mode, Mode::Event);
    assert_eq!(g.state.lifecycle, Lifecycle::Trial);
    assert!(!g.cancel.preempted());
}

/// An undo that fails is kept with the record (the next start tries it
/// again), the alarm is the owner's question, and no new cutover runs
/// until it is undone.
#[test]
fn an_undo_that_fails_is_kept_and_asks_the_owner() {
    let (mut pc, mut g) = ready();
    pc.fail(Call::GuardLogon, "the task did not answer");
    let r = handle(&mut pc, &mut g, cut(false), INIT);
    let detail = format!(
        "cutover of {SHA} failed at GuardLogon: the task did not answer; unwound to trial and \
         event; not undone: GuardLogon: the task did not answer; a guard restart tries again"
    );
    assert!(r.detail.starts_with(&detail), "{}", r.detail);
    assert_eq!(
        g.state.cutover,
        Some(Run {
            build: SHA.into(),
            since: T0,
            begun: vec![CutStep::GuardLogon],
        })
    );
    assert_eq!(pc.autostarts_in, None, "the autostarts are back");
    assert_eq!(g.state.lifecycle, Lifecycle::Trial);
    assert!(g.alarms.iter().last().is_some_and(|a| a.owner_question));
    let r = ask(&mut pc, &mut g, cut(false));
    assert_eq!(
        (r.ok, r.detail),
        (
            false,
            format!(
                "the cutover of {SHA} that was cut off is not fully unwound ([GuardLogon] left): \
                 a guard restart tries again"
            )
        )
    );
}

/// A guard that starts with a cutover cut off (a crash, a power loss
/// between two steps) changes back what it began before anything else, and
/// goes to event, even after a guard restart in live.
#[test]
fn a_cut_off_cutover_is_unwound_at_the_start_and_the_pc_goes_to_event() {
    let dir = tempfile::tempdir().unwrap();
    let mut st = GuardState {
        mode: Mode::Live,
        lifecycle: prod(),
        cutover: Some(Run {
            build: SHA.into(),
            since: T0,
            begun: vec![
                CutStep::Import,
                CutStep::Autostarts,
                CutStep::GuardLogon,
                CutStep::PinChanges,
                CutStep::Lifecycle,
            ],
        }),
        ..GuardState::default()
    };
    st.bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Green));
    st.set_active(SHA);
    let gdir = dir.path().join("guard");
    std::fs::create_dir_all(&gdir).unwrap();
    st.save(&gdir.join(STATE_FILE), T0).unwrap();
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0 + 5));
    let mut pc = FakePc::new(iemmixer_up());
    pc.guard_at_logon = true;
    pc.autostarts_in = Some(export_name(T0));
    pc.server_config = OPEN.into();
    // Booted before the state was written: a guard restart, not a reboot.
    assert_eq!(start(&mut pc, &mut g, 0), Some(Outcome::Done));
    assert_eq!(
        &pc.calls()[..6],
        [
            Call::ServerConfig,
            Call::WriteServerConfig,
            Call::ServerConfig,
            Call::GuardLogon,
            Call::AutostartsOn,
            Call::Procs,
        ]
    );
    assert_eq!(g.state.mode, Mode::Event);
    assert_eq!(g.state.lifecycle, Lifecycle::Trial);
    assert_eq!(g.state.cutover, None);
    assert_eq!(
        (
            pc.guard_at_logon,
            pc.autostarts_in.clone(),
            pc.server_config.as_str()
        ),
        (false, None, FROZEN)
    );
    assert_eq!(
        texts(&g).first(),
        Some(&format!(
            "the cutover of {SHA} was cut off (begun: [Import, Autostarts, GuardLogon, \
             PinChanges, Lifecycle]): unwound to trial; the PC goes to event"
        ))
    );
    assert!(g.alarms.iter().next().is_some_and(|a| !a.owner_question));
    let back = Guard::open(dir.path(), SiteConf::default(), fixed(T0 + 9));
    assert_eq!(
        (back.state.lifecycle, back.state.cutover),
        (Lifecycle::Trial, None)
    );
}

/// The lifecycle is read back from the state file: a save that does not
/// land fails its step, and the unwind reads trial back.
#[test]
fn a_lifecycle_that_does_not_read_back_fails_its_step() {
    let dir = tempfile::tempdir().unwrap();
    // A folder where the state's temp file goes: every save fails.
    std::fs::create_dir_all(dir.path().join("guard").join("guard-state.tmp")).unwrap();
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    g.state.mode = Mode::Dev;
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Green));
    g.state.set_active(SHA);
    let mut pc = FakePc::new(iemmixer_up());
    pc.health(Health::Healthy);
    let r = handle(&mut pc, &mut g, cut(false), INIT);
    assert!(
        r.detail.starts_with(&format!(
            "cutover of {SHA} failed at Lifecycle: the saved lifecycle reads back as Trial, not \
             Prod("
        )),
        "{}",
        r.detail
    );
    assert!(
        r.detail.contains("; unwound to trial and event"),
        "{}",
        r.detail
    );
    assert!(!r.detail.contains("not undone"), "{}", r.detail);
    assert_eq!(g.state.lifecycle, Lifecycle::Trial);
    assert_eq!(pc.server_config, FROZEN);
}

/// An unwind whose event plan needs the owner says so, and the alarm is
/// the owner's question.
#[test]
fn an_unwind_whose_event_plan_needs_the_owner_says_so() {
    let (mut pc, mut g) = ready();
    pc.fail(Call::MemberPage, "refused");
    pc.fail(Call::ReaperFacts, "no answer");
    let r = handle(&mut pc, &mut g, cut(false), INIT);
    let detail = format!(
        "cutover of {SHA} failed at Checks: refused; unwound to trial; the event plan did not \
         end done"
    );
    assert!(r.detail.starts_with(&detail), "{}", r.detail);
    assert_eq!(texts(&g).last(), Some(&detail));
    assert!(g.alarms.iter().last().is_some_and(|a| a.owner_question));
    assert_eq!(g.state.lifecycle, Lifecycle::Trial);
    assert_eq!(g.state.cutover, None);
}
