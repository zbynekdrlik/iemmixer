//! S8 lane 4 (#11): the shadow import's call site against `FakePc` (design
//! note `docs/superpowers/specs/2026-10-10-s8-cutover-rollback-design.md`
//! §3.5): an entry from event runs it after `TuningEnter` and before the
//! data refresh, in prod too (where no refresh runs); it never fails or
//! unwinds an entry, but "ide event" during it pre-empts the entry as any
//! wait; no `shadow` key, or an entry from dev, runs none.

use std::time::{Duration, Instant};

use super::tests::{INIT, SHA, T0, band_up, dev, fixed, prod_on, record, steps, texts};
use super::*;
use crate::bundle::Hil;
use crate::pc::fake::{Call, FakePc};
use crate::plan::Facts;
use crate::proto::Request;

const SAID: &str = "shadow import (event→dev): import writes, 0 site and 0 state difference(s)";

fn with_shadow(facts: Facts) -> FakePc {
    let mut pc = FakePc::new(facts);
    pc.shadows = true;
    pc
}

#[test]
fn a_dev_entry_from_event_runs_the_shadow_before_the_data_refresh() {
    let (mut pc, mut g) = (with_shadow(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    let r = handle(&mut pc, &mut g, dev(), INIT);
    assert!(r.ok, "{r:?}");
    assert_eq!(
        steps(&pc),
        [
            Call::Precheck,
            Call::AppStop,
            Call::ReaperSaveQuit,
            Call::Tuning,
            Call::Shadow,
            Call::Data,
            Call::PrefCheck,
            Call::EngineStart,
            Call::EngineReady,
            Call::EngineArm,
            Call::ServerStart,
            Call::TrayStart,
            Call::Identity,
            Call::RunnerStart,
            Call::TuningDrift,
        ]
    );
    // The guard's report lines follow the result, the shadow's in its place.
    assert!(
        r.detail.starts_with(&format!(
            "dev: done; tuning enter: enter: ok; {SAID}; Dev data refreshed; "
        )),
        "{}",
        r.detail
    );
    // The switch record times it as a step of its own.
    let record = g.state.last_switch.as_ref().unwrap();
    let timed: Vec<Step> = record.steps.iter().map(|s| s.step).collect();
    let at = timed.iter().position(|s| *s == Step::Shadow).unwrap();
    assert_eq!(timed[at - 1], Step::TuningEnter, "{timed:?}");
    assert_eq!(timed[at + 1], Step::Data, "{timed:?}");
    assert!(g.alarms.all().is_empty());
}

/// A shadow that fails is a report line: no alarm, no unwind, the entry
/// goes on to its data refresh.
#[test]
fn a_failed_shadow_never_fails_the_entry() {
    let (mut pc, mut g) = (with_shadow(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    pc.fail(Call::Shadow, "the shadow history could not be written");
    let r = handle(&mut pc, &mut g, dev(), INIT);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Dev);
    assert!(pc.index(Call::Shadow) < pc.index(Call::Data));
    assert!(!pc.called(Call::ReaperStart), "the entry unwound");
    assert_eq!(texts(&g), Vec::<String>::new());
    assert!(
        r.detail.contains(
            "; shadow import not recorded: the shadow history could not be written; Dev data \
             refreshed; "
        ),
        "{}",
        r.detail
    );
}

/// "Ide event" while the shadow runs ends its wait at once and the entry
/// unwinds to REAPER before any data refresh.
#[test]
fn ide_event_during_the_shadow_unwinds_the_entry_at_once() {
    let (mut pc, mut g) = (with_shadow(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    pc.block_until_cancel(Call::Shadow);
    let c = g.cancel.clone();
    let fired = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        c.preempt();
    });
    let began = Instant::now();
    let r = handle(&mut pc, &mut g, dev(), INIT);
    fired.join().unwrap();
    // Far inside the fake's 10 s block: the wait ended at the event.
    assert!(
        began.elapsed() < Duration::from_secs(3),
        "{:?}",
        began.elapsed()
    );
    assert!(!r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Event);
    assert!(!pc.called(Call::Data), "the refresh ran after the event");
    assert!(pc.index(Call::ReaperStart) > pc.index(Call::Shadow));
}

#[test]
fn no_shadow_without_the_key_or_from_dev() {
    // No `shadow` key: no step, the entry's calls as before.
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    assert!(handle(&mut pc, &mut g, dev(), INIT).ok);
    assert!(!pc.called(Call::Shadow));
    // From dev (REAPER saved nothing this entry): none.
    let mut pc = with_shadow(Facts::default());
    let mut g = Guard::for_test(Mode::Dev);
    g.state.pins.current = Some(SHA.into());
    run_switch(&mut pc, &mut g, Mode::Dev, Mode::Dev);
    assert!(!pc.called(Call::Shadow));
    assert!(pc.called(Call::Data), "the dev entry ran");
}

/// The dry run lists the shadow where the entry would run it, and runs
/// none of it.
#[test]
fn the_dry_run_lists_the_shadow_and_runs_nothing() {
    let dry = Request::Dev {
        build: None,
        dry_run: true,
    };
    let (mut pc, mut g) = (with_shadow(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    let r = handle(&mut pc, &mut g, dry.clone(), INIT);
    assert!(r.ok, "{r:?}");
    assert!(
        r.detail.contains("TuningEnter, Shadow, Data, PrefCheck"),
        "{}",
        r.detail
    );
    assert_eq!(pc.mutating_calls(), Vec::<Call>::new());
    let (mut pc, mut g) = (with_shadow(Facts::default()), Guard::for_test(Mode::Dev));
    g.state.pins.current = Some(SHA.into());
    let r = handle(&mut pc, &mut g, dry, INIT);
    assert!(r.ok, "{r:?}");
    assert!(!r.detail.contains("Shadow"), "{}", r.detail);
}

/// In prod no entry refreshes the data (lane 3), but the shadow still runs
/// as a report: the boot's live entry on the pin is an entry from event.
#[test]
fn in_prod_the_shadow_runs_as_a_report_without_a_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let mut st = GuardState {
        lifecycle: prod_on(None, None),
        ..GuardState::default()
    };
    st.bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Green));
    let gdir = dir.path().join("guard");
    std::fs::create_dir_all(&gdir).unwrap();
    st.save(&gdir.join(STATE_FILE), 1_000).unwrap();
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    let mut pc = with_shadow(Facts::default());
    assert_eq!(start(&mut pc, &mut g, 5_000), Some(Outcome::Done));
    assert_eq!(g.state.mode, Mode::Live);
    assert!(!pc.called(Call::Data), "a prod entry refreshed the data");
    assert!(pc.index(Call::Tuning) < pc.index(Call::Shadow));
    assert!(pc.index(Call::Shadow) < pc.index(Call::EngineStart));
}
