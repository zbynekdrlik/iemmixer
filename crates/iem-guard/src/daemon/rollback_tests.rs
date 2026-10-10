//! S8 lane 3 (#11): the rollback against `FakePc` (design note
//! `docs/superpowers/specs/2026-10-10-s8-cutover-rollback-design.md` §3.3):
//! its steps in their order, REAPER on the export or on the original, what
//! is left and what a restart continues, and what `iemmode event` means in
//! each lifecycle (the button rolls back in prod, "ide event" never does).

use std::sync::Arc;
use std::time::Duration;

use super::tests::{
    INIT, OTHER, SHA, T0, ask, band_up, fixed, iemmixer_up, prod_on, record, status_reply, texts,
};
use super::*;
use crate::bundle::Hil;
use crate::cutover::export_name;
use crate::lifecycle::{Lifecycle, ROLLING_BACK};
use crate::pc::fake::{Call, FakePc};
use crate::plan::{Facts, Health};
use crate::proto::Request;
use crate::rollback::{Files, ON_EXPORT, ON_ORIGINAL, RollStep, Run};

const OPEN: &str = "port = 80\npin_changes = true\n";
const FROZEN: &str = "port = 80\npin_changes = false\n";

fn back(dry_run: bool) -> Request {
    Request::Rollback { dry_run }
}

fn event(signal: bool) -> Request {
    Request::Event {
        dry_run: false,
        signal,
    }
}

/// The files once the export took the project's place.
fn on_export() -> Files {
    Files {
        project: true,
        export: false,
        kept: true,
    }
}

/// The files with the original back and the export under its own name.
fn on_original() -> Files {
    Files {
        project: true,
        export: true,
        kept: false,
    }
}

/// Prod live on `SHA` as the cutover at `T0` left it: iemmixer up, the
/// predecessor's autostarts in `autostarts-<T0>`, the guard at the logon,
/// `pin_changes` open.
fn prod_live() -> (FakePc, Guard) {
    let mut pc = FakePc::new(iemmixer_up());
    pc.server_config = OPEN.into();
    pc.autostarts_in = Some(export_name(T0));
    pc.guard_at_logon = true;
    let mut g = Guard::for_test(Mode::Live);
    g.state.lifecycle = prod_on(None, None);
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Green));
    g.state.set_active(SHA);
    (pc, g)
}

/// What a finished rollback leaves: trial, event, no record, REAPER and
/// the app up, the predecessor's autostarts back, the guard no longer at
/// the logon, `pin_changes` closed.
fn assert_rolled_back(pc: &FakePc, g: &Guard) {
    assert_eq!(g.state.lifecycle, Lifecycle::Trial);
    assert_eq!(g.state.rollback, None);
    assert_eq!(g.state.mode, Mode::Event);
    assert!(pc.facts.reaper && pc.facts.reaper_holds_module && pc.facts.app);
    assert!(!pc.facts.engine && !pc.facts.server && !pc.facts.tray);
    assert_eq!(pc.autostarts_in, None);
    assert!(!pc.guard_at_logon);
    assert_eq!(pc.server_config, FROZEN);
}

#[test]
fn a_rollback_ends_in_trial_with_reaper_on_the_export() {
    let (mut pc, mut g) = prod_live();
    let r = handle(&mut pc, &mut g, back(false), INIT);
    assert!(r.ok, "{r:?}");
    let done = format!(
        "rollback done: trial, event; {ON_EXPORT}; the original project is kept as \
         before-rollback-{T0}"
    );
    // The guard's report lines of the steps follow the result.
    assert!(r.detail.starts_with(&done), "{}", r.detail);
    assert_rolled_back(&pc, &g);
    assert_eq!(pc.project, on_export());
    // The engine stops (it saves its state) before the export reads it;
    // the export takes the project's place before REAPER starts; REAPER
    // comes back before the persistent changes; the autostarts before the
    // guard leaves the logon. No data comes from the predecessor.
    let order = [
        Call::EngineStop,
        Call::ExportProject,
        Call::SwapProject,
        Call::ReaperStart,
        Call::AppStart,
        Call::AutostartsOn,
        Call::GuardLogon,
        Call::WriteServerConfig,
    ];
    let at: Vec<usize> = order.iter().map(|c| pc.index(*c)).collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "{:?}", pc.calls());
    assert_eq!(pc.count(Call::ReaperStart), 1);
    assert!(!pc.called(Call::Data) && !pc.called(Call::ReaperSaveQuit));
    assert_eq!(
        texts(&g),
        Vec::<String>::new(),
        "a clean rollback alarms nothing"
    );
}

/// The cutover, then the rollback: the PC is as before the cutover but for
/// the band's data, which REAPER now holds (the drill in small).
#[test]
fn a_cutover_then_a_rollback_gives_back_the_trial() {
    let mut pc = FakePc::new(iemmixer_up());
    pc.health(Health::Healthy);
    let mut g = Guard::for_test(Mode::Dev);
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Green));
    g.state.set_active(SHA);
    let cut = Request::Cutover {
        build: SHA.into(),
        dry_run: false,
    };
    assert!(handle(&mut pc, &mut g, cut, INIT).ok);
    assert!(matches!(g.state.lifecycle, Lifecycle::Prod(_)));
    let r = ask(&mut pc, &mut g, back(false));
    assert!(r.ok, "{r:?}");
    assert!(r.detail.contains(ON_EXPORT), "{}", r.detail);
    assert_rolled_back(&pc, &g);
}

#[test]
fn before_the_cutover_there_is_no_rollback_and_a_dry_run_changes_nothing() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    let r = handle(&mut pc, &mut g, back(false), INIT);
    assert!(!r.ok);
    assert!(
        r.detail.starts_with("there is no cutover to roll back"),
        "{}",
        r.detail
    );
    assert_eq!(pc.mutating_calls(), Vec::<Call>::new());
    let (mut pc, mut g) = prod_live();
    let r = handle(&mut pc, &mut g, back(true), INIT);
    assert!(r.ok, "{r:?}");
    let head = format!("dry run: rollback from the pin {SHA}: RollingBack saved; Stop");
    assert!(r.detail.starts_with(&head), "{}", r.detail);
    assert!(r.detail.contains(&export_name(T0)), "{}", r.detail);
    assert_eq!(pc.mutating_calls(), Vec::<Call>::new());
    assert_eq!(g.state.lifecycle, prod_on(None, None));
    assert_eq!(g.state.rollback, None);
}

/// Program spec §4.3 and the lane's decision: in prod the engineer's
/// button (plain `iemmode event`) is the rollback; the owner's "ide event"
/// (`--signal`) never is.
#[test]
fn in_prod_the_button_rolls_back_and_ide_event_never_does() {
    let (mut pc, mut g) = prod_live();
    let r = handle(&mut pc, &mut g, event(false), INIT);
    assert!(r.ok, "{r:?}");
    assert!(r.detail.starts_with("rollback done"), "{}", r.detail);
    assert_rolled_back(&pc, &g);
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
    assert!(pc.called(Call::ReaperStart) && !pc.called(Call::ExportProject));
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
    // …in event (a red pin at the boot): REAPER's checks, nothing else.
    let mut pc = FakePc::new(band_up());
    let mut g = Guard::for_test(Mode::Event);
    g.state.lifecycle = prod_on(None, None);
    let r = handle(&mut pc, &mut g, event(true), INIT);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.lifecycle, prod_on(None, None));
    assert!(!pc.called(Call::EngineStart) && !pc.called(Call::ExportProject));
    // Before the cutover both are the event plan, as always.
    for signal in [false, true] {
        let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
        let r = handle(&mut pc, &mut g, event(signal), INIT);
        assert!(r.ok, "{r:?}");
        assert_eq!(
            (g.state.mode, g.state.lifecycle.clone()),
            (Mode::Event, Lifecycle::Trial)
        );
        assert!(!pc.called(Call::ExportProject));
    }
}

/// Prod lost to trial (an older guard took over, an unreadable state)
/// leaves the cutover's export never restored, the autostarts disabled,
/// the guard at the logon and `pin_changes = true`: every entry fails at
/// `ServerStart`. `iemmode rollback` runs there as a repair, its steps and
/// guarantees as in prod: REAPER at the end, the autostarts back from the
/// newest export never restored, the logon trigger off, pin_changes closed
/// (S8 lane 5, the cross-lane review's finding 2b). Without either it is
/// refused as before.
#[test]
fn a_repair_rollback_runs_in_trial_after_a_lost_prod() {
    let mut pc = FakePc::new(iemmixer_up());
    pc.server_config = OPEN.into();
    pc.autostarts_in = Some(export_name(T0));
    pc.guard_at_logon = true;
    let mut g = Guard::for_test(Mode::Live);
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Green));
    g.state.set_active(SHA);
    let r = handle(&mut pc, &mut g, back(true), INIT);
    assert!(r.ok, "{r:?}");
    assert!(r.detail.starts_with("dry run: "), "{}", r.detail);
    assert!(r.detail.contains(&export_name(T0)), "{}", r.detail);
    assert_eq!(pc.mutating_calls(), Vec::<Call>::new());
    let r = ask(&mut pc, &mut g, back(false));
    assert!(r.ok, "{r:?}");
    assert!(r.detail.contains(ON_EXPORT), "{}", r.detail);
    assert_rolled_back(&pc, &g);
    // Only pin_changes left open: the rollback closes it, REAPER runs.
    let mut pc = FakePc::new(iemmixer_up());
    pc.server_config = OPEN.into();
    let mut g = Guard::for_test(Mode::Live);
    let r = handle(&mut pc, &mut g, back(false), INIT);
    assert!(r.detail.starts_with("rollback done"), "{r:?}");
    assert_eq!(pc.server_config, FROZEN);
    assert!(pc.facts.reaper && g.state.lifecycle == Lifecycle::Trial);
    assert_eq!(g.state.mode, Mode::Event);
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
    }
}

/// A failed export leaves the original in place: REAPER on it, the
/// rollback done, and the owner hears that iemmixer's changes stayed
/// behind.
#[test]
fn a_failed_export_brings_reaper_back_on_the_original_with_an_alarm() {
    let (mut pc, mut g) = prod_live();
    pc.fail(Call::ExportProject, "iem-migrate.exe ended with Some(2)");
    let r = handle(&mut pc, &mut g, back(false), INIT);
    assert!(r.ok, "{r:?}");
    assert!(
        r.detail.starts_with(&format!(
            "rollback done: trial, event; {ON_ORIGINAL}; the band's data was not exported"
        )),
        "{}",
        r.detail
    );
    assert_rolled_back(&pc, &g);
    assert!(!pc.called(Call::SwapProject));
    assert!(!pc.project.kept);
    let alarm = g.alarms.last().unwrap();
    assert!(
        alarm.owner_question && alarm.text.contains("not carried back"),
        "{alarm:?}"
    );
}

/// REAPER that cannot open the export is quit, the original goes back in
/// the project's place, and REAPER starts on it: REAPER runs at the end.
#[test]
fn reaper_that_cannot_open_the_export_comes_back_on_the_original() {
    let (mut pc, mut g) = prod_live();
    pc.export_unloadable = true;
    let r = handle(&mut pc, &mut g, back(false), INIT);
    assert!(r.ok, "{r:?}");
    assert!(r.detail.contains(ON_ORIGINAL), "{}", r.detail);
    assert!(
        r.detail.contains("REAPER could not open the export"),
        "{}",
        r.detail
    );
    assert_rolled_back(&pc, &g);
    assert_eq!(
        pc.project,
        on_original(),
        "the export is kept under its own name"
    );
    assert_eq!(pc.count(Call::ReaperStart), 2);
    assert_eq!(pc.count(Call::SwapProject), 2);
    assert!(pc.index(Call::ReaperSaveQuit) > pc.index(Call::ReaperFacts));
    assert!(
        g.alarms
            .iter()
            .any(|a| a.owner_question && a.text.contains(ON_ORIGINAL))
    );
}

/// In prod the button that cancels a running dev or live entry finds REAPER
/// started by that entry's unwind, on the original project: the rollback
/// saves and quits it gracefully, puts the export in the project's place
/// and starts REAPER on it (S8 lane 5, finding 5). A REAPER that does not
/// quit keeps running on the original: REAPER runs at the end either way.
#[test]
fn a_reaper_an_unwind_started_is_quit_so_the_export_takes_its_place() {
    let (mut pc, mut g) = prod_live();
    pc.facts = band_up();
    g.state.mode = Mode::Event;
    let r = handle(&mut pc, &mut g, event(false), INIT);
    assert!(r.ok, "{r:?}");
    assert!(r.detail.contains(ON_EXPORT), "{}", r.detail);
    assert_rolled_back(&pc, &g);
    assert_eq!(pc.project, on_export());
    let order = [
        Call::ExportProject,
        Call::ReaperSaveQuit,
        Call::SwapProject,
        Call::ReaperStart,
    ];
    let at: Vec<usize> = order.iter().map(|c| pc.index(*c)).collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "{:?}", pc.calls());
    assert_eq!(texts(&g), Vec::<String>::new(), "nothing fell back");
    // REAPER that does not quit: the original stays in place, REAPER runs.
    let (mut pc, mut g) = prod_live();
    pc.facts = band_up();
    g.state.mode = Mode::Event;
    pc.fail(Call::ReaperSaveQuit, "REAPER did not quit within 30 s");
    let r = handle(&mut pc, &mut g, event(false), INIT);
    assert!(r.ok, "{r:?}");
    assert!(r.detail.contains(ON_ORIGINAL), "{}", r.detail);
    assert!(
        r.detail.contains("REAPER did not quit within 30 s"),
        "{}",
        r.detail
    );
    assert_rolled_back(&pc, &g);
    assert!(!pc.called(Call::SwapProject) || !pc.project.kept);
    assert!(g.alarms.last().unwrap().owner_question);
}

/// The autostarts that do not come back keep the guard at the logon (the
/// next boot starts it, which continues) and the rollback left; REAPER
/// runs meanwhile. Another `iemmode rollback` finishes it without touching
/// REAPER again.
#[test]
fn autostarts_that_do_not_come_back_keep_the_logon_trigger_and_the_rollback() {
    let (mut pc, mut g) = prod_live();
    pc.fail(Call::AutostartsOn, "the cutover task did not answer");
    let r = handle(&mut pc, &mut g, back(false), INIT);
    assert!(!r.ok, "{r:?}");
    assert!(
        r.detail.starts_with(
            "rollback not finished: Autostarts: the cutover task did not answer; GuardLogon: \
             kept while the predecessor's autostarts are not back"
        ),
        "{}",
        r.detail
    );
    assert_eq!(g.state.lifecycle, Lifecycle::RollingBack);
    assert!(pc.guard_at_logon);
    assert!(pc.facts.reaper && g.state.mode == Mode::Event);
    assert_eq!(pc.server_config, FROZEN, "the steps after it still ran");
    let run = g.state.rollback.clone().unwrap();
    assert_eq!(
        run.done,
        [
            RollStep::Stop,
            RollStep::Export,
            RollStep::Project,
            RollStep::Reaper,
            RollStep::PinChanges
        ]
    );
    assert!(g.alarms.last().unwrap().owner_question);
    assert!(status_text(&g).contains(&format!("rollback since {T0} from the pin {SHA}")));
    // No entry while it is left; no activation either.
    let r = ask(&mut pc, &mut g, super::tests::dev());
    assert_eq!((r.ok, r.detail.as_str()), (false, ROLLING_BACK));
    let r = ask(&mut pc, &mut g, Request::Activate { sha: SHA.into() });
    assert!(
        !r.ok && r.detail.starts_with("the rollback from the pin"),
        "{r:?}"
    );
    pc.heal(Call::AutostartsOn);
    let starts = pc.count(Call::ReaperStart);
    let r = ask(&mut pc, &mut g, back(false));
    assert!(r.ok, "{r:?}");
    assert_rolled_back(&pc, &g);
    assert_eq!(pc.count(Call::ReaperStart), starts);
}

/// A healthy engine that does not stop keeps serving: no export, no
/// REAPER; the rollback stops and waits, the owner asked.
#[test]
fn an_engine_that_does_not_stop_stops_the_rollback_and_keeps_serving() {
    let (mut pc, mut g) = prod_live();
    pc.fail(
        Call::EngineStop,
        "no DriverReleased or DriverParked within 10 s",
    );
    pc.health(Health::Healthy);
    let r = handle(&mut pc, &mut g, back(false), INIT);
    assert!(!r.ok, "{r:?}");
    assert!(
        r.detail.starts_with(
            "rollback stopped: Stop: EngineStop: no DriverReleased or DriverParked within 10 s; \
             the engine is healthy and iemmixer keeps serving"
        ),
        "{}",
        r.detail
    );
    assert!(!pc.called(Call::ExportProject) && !pc.called(Call::ReaperStart));
    assert_eq!(g.state.lifecycle, Lifecycle::RollingBack);
    assert_eq!(
        g.state.rollback.as_ref().map(|r| r.done.clone()),
        Some(Vec::new())
    );
    assert!(g.alarms.last().unwrap().owner_question);
    // A dead engine may hold the card: no REAPER either.
    let (mut pc, mut g) = prod_live();
    pc.fail(Call::EngineStop, "the pipe is gone");
    pc.health(Health::Dead);
    let r = handle(&mut pc, &mut g, back(false), INIT);
    assert!(
        !r.ok && r.detail.contains("no REAPER while the card may be held"),
        "{r:?}"
    );
    assert!(!pc.called(Call::ReaperStart));
    // The mode is event, as the event plan leaves it: the watch starts no
    // engine again.
    assert_eq!(g.state.mode, Mode::Event);
}

/// A guard that starts with a rollback left continues it: its stops and
/// its event plan again, then the steps not done.
#[test]
fn a_starting_guard_continues_a_rollback_left() {
    let dir = tempfile::tempdir().unwrap();
    let gdir = dir.path().join("guard");
    std::fs::create_dir_all(&gdir).unwrap();
    let mut st = GuardState {
        lifecycle: Lifecycle::RollingBack,
        rollback: Some(Run {
            pin: SHA.into(),
            since: Some(T0),
            at: T0,
            done: vec![
                RollStep::Stop,
                RollStep::Export,
                RollStep::Project,
                RollStep::Reaper,
            ],
            exported: true,
            on_export: true,
        }),
        ..GuardState::default()
    };
    st.bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Green));
    st.save(&gdir.join(STATE_FILE), 1_000).unwrap();
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0 + 60));
    let mut pc = FakePc::new(Facts::default());
    pc.project = on_export();
    pc.server_config = OPEN.into();
    pc.autostarts_in = Some(export_name(T0));
    pc.guard_at_logon = true;
    assert_eq!(start(&mut pc, &mut g, 5_000), Some(Outcome::Done));
    assert_rolled_back(&pc, &g);
    assert_eq!(
        pc.count(Call::ReaperStart),
        1,
        "the rollback's event plan is the start's"
    );
    assert!(!pc.called(Call::ExportProject) && !pc.called(Call::SwapProject));
    let (back, _) = GuardState::load(&gdir.join(STATE_FILE));
    assert_eq!((back.lifecycle, back.rollback), (Lifecycle::Trial, None));
    // Rolling back with its record lost: again, without the cutover's
    // export; the guard keeps its logon trigger, the owner hears why.
    let dir = tempfile::tempdir().unwrap();
    let gdir = dir.path().join("guard");
    std::fs::create_dir_all(&gdir).unwrap();
    let mut st = GuardState {
        lifecycle: Lifecycle::RollingBack,
        ..GuardState::default()
    };
    st.save(&gdir.join(STATE_FILE), 1_000).unwrap();
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    let mut pc = FakePc::new(Facts::default());
    pc.server_config = OPEN.into();
    pc.guard_at_logon = true;
    assert_eq!(start(&mut pc, &mut g, 5_000), Some(Outcome::Done));
    assert_eq!(
        (g.state.lifecycle.clone(), g.state.mode),
        (Lifecycle::Trial, Mode::Event)
    );
    assert!(pc.guard_at_logon && !pc.called(Call::AutostartsOn));
    assert!(
        g.alarms
            .iter()
            .any(|a| a.owner_question && a.text.contains("not restored"))
    );
}

/// The record is read back before any step: one that does not save
/// refuses the rollback and nothing changes.
#[test]
fn a_record_that_does_not_save_refuses_the_rollback() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("guard").join("guard-state.tmp")).unwrap();
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    g.state.mode = Mode::Live;
    g.state.lifecycle = prod_on(None, None);
    let mut pc = FakePc::new(iemmixer_up());
    let r = handle(&mut pc, &mut g, back(false), INIT);
    assert!(!r.ok);
    assert!(
        r.detail.starts_with("the rollback's record does not save"),
        "{}",
        r.detail
    );
    assert_eq!(pc.mutating_calls(), Vec::<Call>::new());
    assert_eq!(
        (g.state.lifecycle.clone(), g.state.rollback.clone()),
        (prod_on(None, None), None)
    );
}

/// What a finished rollback leaves is saved: a new guard reads trial.
#[test]
fn the_rollback_is_saved_for_the_next_guard() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    g.state.mode = Mode::Live;
    g.state.lifecycle = prod_on(Some(OTHER), None);
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Green));
    g.state.set_active(SHA);
    let mut pc = FakePc::new(iemmixer_up());
    pc.server_config = OPEN.into();
    let r = handle(&mut pc, &mut g, back(false), INIT);
    assert!(r.ok, "{r:?}");
    let next = Guard::open(dir.path(), SiteConf::default(), fixed(T0 + 5));
    assert_eq!(
        (
            next.state.lifecycle.clone(),
            next.state.rollback.clone(),
            next.state.mode
        ),
        (Lifecycle::Trial, None, Mode::Event)
    );
    assert!(!status_reply(&next).contains("prod since"));
}

/// "ide event" that leaves a healthy prod live as it is clears the
/// pre-emption the pipe set as it routed it: the next switch (a maintenance
/// entry) is not pre-empted back to REAPER (the lane's review).
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

/// The rollback's stops run on a token of their own: an "ide event" routed
/// while they run (it pre-empts the pipe's token) does not cut them short;
/// it waits behind the rollback.
#[test]
fn the_stops_finish_whatever_an_ide_event_routed_meanwhile() {
    let (mut pc, mut g) = prod_live();
    pc.waits_see_preemption = true;
    pc.preempt_at = Some((Call::EngineStop, g.cancel.clone()));
    let r = handle(&mut pc, &mut g, back(false), INIT);
    assert!(r.ok, "{r:?}");
    assert_rolled_back(&pc, &g);
    assert_eq!(texts(&g), Vec::<String>::new());
}

/// A swap that fails both ways leaves the original where it is: REAPER on
/// it, one more try to settle it before REAPER's start, the owner told.
#[test]
fn a_swap_that_fails_both_ways_brings_reaper_on_the_original() {
    let (mut pc, mut g) = prod_live();
    pc.fail(Call::SwapProject, "the export is held by another process");
    let r = handle(&mut pc, &mut g, back(false), INIT);
    assert!(r.ok, "{r:?}");
    assert!(r.detail.contains(ON_ORIGINAL), "{}", r.detail);
    assert!(
        r.detail
            .contains("the export could not take the project's place"),
        "{}",
        r.detail
    );
    assert_rolled_back(&pc, &g);
    assert_eq!(pc.count(Call::SwapProject), 3);
    assert_eq!(pc.count(Call::ReaperStart), 1);
    assert_eq!(pc.project, on_original());
    assert!(g.alarms.last().unwrap().owner_question);
}

/// An event plan that ends with REAPER but needs the owner (the app does
/// not answer, #10) ends the rollback in trial, not ok.
#[test]
fn an_event_plan_that_needs_the_owner_ends_the_rollback_not_ok() {
    let (mut pc, mut g) = prod_live();
    pc.fail(Call::AppAnswers, "the app does not answer");
    let r = handle(&mut pc, &mut g, back(false), INIT);
    assert!(!r.ok, "{r:?}");
    assert!(
        r.detail.contains("the event plan needs the owner"),
        "{}",
        r.detail
    );
    assert_eq!(
        (g.state.lifecycle.clone(), g.state.rollback.clone()),
        (Lifecycle::Trial, None)
    );
    assert!(pc.facts.reaper);
}

/// The guard's logon trigger that does not go off keeps the rollback left.
#[test]
fn a_logon_trigger_that_does_not_go_off_keeps_the_rollback_left() {
    let (mut pc, mut g) = prod_live();
    pc.fail(Call::GuardLogon, "the cutover task did not answer");
    let r = handle(&mut pc, &mut g, back(false), INIT);
    assert!(!r.ok);
    assert!(
        r.detail
            .starts_with("rollback not finished: GuardLogon: the cutover task did not answer"),
        "{}",
        r.detail
    );
    assert_eq!(g.state.lifecycle, Lifecycle::RollingBack);
    assert_eq!(
        (pc.autostarts_in.clone(), pc.server_config.as_str()),
        (None, FROZEN)
    );
}

/// In prod the button runs after a switch in progress, never answered as
/// its end: it pre-empts a dev or live entry, never an event plan; "ide
/// event" keeps the routing it had, and before the cutover so does the
/// button. Queued before a switch ran, it is not stale.
#[test]
fn in_prod_the_button_runs_after_a_switch_in_progress() {
    let (mut pc, mut g) = prod_live();
    g.save();
    for (running, preempts) in [(Mode::Live, true), (Mode::Dev, true), (Mode::Event, false)] {
        g.cancel.clear();
        g.shared.update(|v| v.running = Some(running));
        assert!(
            matches!(g.shared.route(&event(false)), Route::Queue(_)),
            "{running:?}"
        );
        assert_eq!(g.cancel.preempted(), preempts, "{running:?}");
    }
    g.shared.update(|v| v.running = Some(Mode::Live));
    assert!(matches!(g.shared.route(&event(true)), Route::AwaitEnd(_)));
    let trial = Guard::for_test(Mode::Live);
    trial.shared.update(|v| v.running = Some(Mode::Live));
    assert!(matches!(
        trial.shared.route(&event(false)),
        Route::AwaitEnd(_)
    ));
    g.cancel.clear();
    g.shared.update(|v| {
        v.running = None;
        v.epoch += 1;
    });
    let r = handle(&mut pc, &mut g, event(false), INIT);
    assert!(r.ok && r.detail.starts_with("rollback done"), "{r:?}");
}

/// In prod "ide event" routed while a switch to live runs (an in-flight
/// iempc command's own `iemmode event --signal` next to the owner's) waits
/// for it and never pre-empts it: after the cutover live is the band's
/// system, so the entry goes on and its end counts as done (S8 lane 5,
/// finding 1). Before the cutover it pre-empts as before, and live is no
/// event done.
#[test]
fn in_prod_ide_event_waits_for_a_switch_to_live() {
    let (_, mut g) = prod_live();
    g.save();
    g.shared.update(|v| v.running = Some(Mode::Live));
    assert!(matches!(g.shared.route(&event(true)), Route::AwaitEnd(_)));
    assert!(
        !g.cancel.preempted(),
        "a switch to live in prod is never pre-empted"
    );
    g.shared.update(|v| {
        v.running = None;
        v.last = Some(Outcome::Done);
    });
    let r = g.shared.await_end("waited", Duration::ZERO);
    assert!(r.ok && r.mode == Mode::Live, "{r:?}");
    assert!(r.detail.starts_with("waited; live: "), "{}", r.detail);
    let trial = Guard::for_test(Mode::Live);
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

/// `iemmode rollback` is queued behind the start's checks and busy during
/// any other switch; an offline activation is refused while a rollback is
/// left.
#[test]
fn the_rollback_is_routed_like_an_entry() {
    let g = Guard::for_test(Mode::Event);
    g.shared.update(|v| {
        v.running = Some(Mode::Event);
        v.start_checks = true;
    });
    assert!(matches!(g.shared.route(&back(false)), Route::Queue(_)));
    g.shared.update(|v| v.start_checks = false);
    match g.shared.route(&back(false)) {
        Route::Now(r) => assert_eq!((r.ok, r.detail.as_str()), (false, "busy")),
        other => panic!("{other:?}"),
    }
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    g.state.lifecycle = Lifecycle::RollingBack;
    g.state.rollback = Some(Run {
        pin: SHA.into(),
        since: Some(T0),
        at: T0,
        done: Vec::new(),
        exported: false,
        on_export: false,
    });
    let r = activate_offline(&mut pc, &mut g, Some(()), SHA);
    assert!(
        !r.ok && r.detail.starts_with("the rollback from the pin"),
        "{r:?}"
    );
    assert_eq!(pc.mutating_calls(), Vec::<Call>::new());
}
