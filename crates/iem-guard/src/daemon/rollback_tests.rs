//! S8 lane 3 (#11): the rollback against `FakePc` (design note
//! `docs/superpowers/specs/2026-10-10-s8-cutover-rollback-design.md` §3.3):
//! its steps in their order, REAPER on the export or on the original, what
//! is left and what a restart continues, and what `iemmode event` means in
//! each lifecycle (the button rolls back in prod, "ide event" never does).

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
    assert_eq!(r.detail, done);
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
