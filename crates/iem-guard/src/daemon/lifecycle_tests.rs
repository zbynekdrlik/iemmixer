//! S8 (#11): the lifecycle's call sites against `FakePc` (design note
//! `docs/superpowers/specs/2026-10-10-s8-cutover-rollback-design.md` §3.1,
//! §3.4): an entry or an activation never promotes the pin; the active
//! bundle across a restart; prod's boot, entries, maintenance and crash
//! loops, driven through test-only state (nothing in this lane sets prod).

use std::time::{Duration, Instant};

use super::tests::{
    INIT, OTHER, SHA, T0, ask, band_up, fixed, iemmixer_up, prod_on, record, texts,
};
use super::*;
use crate::bundle::{Hil, Pins};
use crate::install;
use crate::lifecycle::{Lifecycle, Prod};
use crate::pc::Kid;
use crate::pc::fake::{Call, FakePc};
use crate::plan::Facts;
use crate::proto::Request;

/// The pins as an earlier guard left them: `OTHER`, nothing before it.
fn other_pinned() -> Pins {
    Pins {
        current: Some(OTHER.into()),
        previous: None,
    }
}

/// The legacy record as this guard writes it (lane 2, the PC on
/// 2026-10-10): `pins.current` mirrors the active bundle and `pins.previous`
/// the way back, so an older guard that takes over runs the active bundle.
/// The pin itself is the lifecycle's.
fn mirrored(active: &str, way_back: Option<&str>) -> Pins {
    Pins {
        current: Some(active.into()),
        previous: way_back.map(str::to_owned),
    }
}

/// The pin bug (design §3.4): every dev or live entry promoted its build to
/// the pin before any HIL result. An entry runs its build and leaves the
/// pin (the lifecycle's) as it was: a dev entry with a build, and a live
/// trial. The legacy pins only mirror the active bundle and the way back.
#[test]
fn an_entry_never_promotes_the_pin() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    g.state.pins = other_pinned();
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "dev", Hil::Pending));
    let dev = Request::Dev {
        build: Some(SHA.into()),
        dry_run: false,
    };
    let r = handle(&mut pc, &mut g, dev, INIT);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Dev);
    assert_eq!(g.state.lifecycle, Lifecycle::Trial, "a dev entry pinned");
    assert_eq!(
        g.state.pins,
        mirrored(SHA, Some(OTHER)),
        "the legacy pins mirror the active bundle and the way back"
    );
    assert_eq!(pc.bundle.as_deref(), Some(SHA), "the entry runs its build");
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins = other_pinned();
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Green));
    let trial = Request::Live {
        build: Some(SHA.into()),
        trial: true,
        dry_run: false,
    };
    let r = handle(&mut pc, &mut g, trial, INIT);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Live);
    assert_eq!(g.state.lifecycle, Lifecycle::Trial, "a trial pinned");
    assert_eq!(g.state.pins, mirrored(SHA, Some(OTHER)));
    assert_eq!(pc.bundle.as_deref(), Some(SHA));
}

/// `activate` makes its bundle the active one for dev (the engine and the
/// server run it, the bundle before it keeps its Defender exclusions) and
/// leaves the pin as it was; the legacy pins mirror the active bundle.
#[test]
fn an_activation_never_promotes_the_pin() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    g.state.mode = Mode::Dev;
    let zip = install::tests::good_zip(dir.path(), SHA);
    assert!(install_bundle(&mut g, &zip).0);
    g.state.pins = other_pinned();
    let mut pc = FakePc::new(iemmixer_up());
    let r = handle(&mut pc, &mut g, Request::Activate { sha: SHA.into() }, INIT);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.lifecycle, Lifecycle::Trial, "activate pinned");
    assert_eq!(g.state.pins, mirrored(SHA, Some(OTHER)));
    assert_eq!(pc.bundle.as_deref(), Some(SHA));
    assert_eq!(pc.excluded, [(SHA.to_owned(), vec![OTHER.to_owned()])]);
}

// ---- the active bundle across a restart ----

/// The entry's build is the active bundle, saved: a guard restart (no
/// reboot) runs it again, as before S8 when the entry pinned it.
#[test]
fn an_entry_s_build_stays_the_active_bundle_after_a_guard_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    g.state.pins = other_pinned();
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "dev", Hil::Pending));
    let mut pc = FakePc::new(Facts::default());
    let dev = Request::Dev {
        build: Some(SHA.into()),
        dry_run: false,
    };
    assert!(handle(&mut pc, &mut g, dev, INIT).ok);
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    let mut pc = FakePc::new(iemmixer_up());
    assert_eq!(start(&mut pc, &mut g, 0), None);
    assert_eq!(g.state.mode, Mode::Dev);
    assert_eq!(pc.bundle.as_deref(), Some(SHA));
    assert_eq!(g.state.pins, mirrored(SHA, Some(OTHER)));
}

// ---- prod (test-only state: nothing in this lane sets it) ----

/// A third build: the maintenance session's.
const NEW: &str = "fedcba9876543210fedcba9876543210fedcba98";

/// A guard whose saved state is `st`, written at `at`, with `SHA`, `OTHER`
/// and `NEW` installed green from main.
fn saved(dir: &Path, mut st: GuardState, at: u64) -> Guard {
    for sha in [SHA, OTHER, NEW] {
        st.bundles
            .insert(sha.into(), record(sha, "main", Hil::Green));
    }
    let gdir = dir.join("guard");
    std::fs::create_dir_all(&gdir).unwrap();
    st.save(&gdir.join(STATE_FILE), at).unwrap();
    Guard::open(dir, SiteConf::default(), fixed(T0))
}

/// `shas` installed as green main builds (a pin a fallback may go to, G8).
fn green_main(g: &mut Guard, shas: &[&str]) {
    for sha in shas {
        g.state
            .bundles
            .insert((*sha).into(), record(sha, "main", Hil::Green));
    }
}

/// G8 on the crash path: a crash loop in maintenance whose pin HIL reported
/// red goes back to REAPER, never live on it (the review of lane 1).
#[test]
fn a_crash_loop_in_maintenance_on_a_red_pin_goes_back_to_reaper() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Dev));
    g.state.lifecycle = prod_on(None, Some(NEW));
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Red));
    pc.exited = vec![(Kid::Engine, Some(70)); 3];
    tick(&mut pc, &mut g, Instant::now());
    assert_eq!(g.state.mode, Mode::Event);
    assert!(pc.called(Call::ReaperStart) && !pc.called(Call::EngineStart));
    assert_eq!(g.state.lifecycle, prod_on(None, None));
    assert_eq!(
        texts(&g),
        [format!(
            "the engine crashed 3 times in 10 min in maintenance, and {SHA}: HIL Red; live \
             needs green: back to REAPER"
        )]
    );
}

#[test]
fn in_prod_a_reboot_goes_live_on_the_pin() {
    let dir = tempfile::tempdir().unwrap();
    let st = GuardState {
        lifecycle: prod_on(None, None),
        active: Some(OTHER.into()),
        ..GuardState::default()
    };
    let mut g = saved(dir.path(), st, 1_000);
    let mut pc = FakePc::new(Facts::default());
    assert_eq!(start(&mut pc, &mut g, 5_000), Some(Outcome::Done));
    assert_eq!(g.state.mode, Mode::Live);
    assert_eq!(g.state.active_bundle(), Some(SHA));
    assert_eq!(pc.bundle.as_deref(), Some(SHA));
    assert!(pc.called(Call::EngineStart));
    assert!(!pc.called(Call::ReaperStart) && !pc.called(Call::RunnerStart));
    // A switch of its own (event → live), not the start's checks.
    let v = g.shared.view();
    assert_eq!(
        (v.start_checks, v.began),
        (false, Some((Mode::Event, Mode::Live)))
    );
    assert_eq!(g.state.lifecycle, prod_on(None, None));
    // A reboot during maintenance on a green main build ends it: live on
    // that build, which is the pin from then on (saved).
    let dir = tempfile::tempdir().unwrap();
    let st = GuardState {
        lifecycle: prod_on(None, Some(NEW)),
        active: Some(NEW.into()),
        ..GuardState::default()
    };
    let mut g = saved(dir.path(), st, 1_000);
    let mut pc = FakePc::new(Facts::default());
    assert_eq!(start(&mut pc, &mut g, 5_000), Some(Outcome::Done));
    assert_eq!(g.state.mode, Mode::Live);
    assert_eq!(pc.bundle.as_deref(), Some(NEW));
    let promoted = Lifecycle::Prod(Prod {
        since: T0,
        pin: NEW.into(),
        previous: Some(SHA.into()),
        maintenance: None,
    });
    assert_eq!(g.state.lifecycle, promoted);
    let (back, _) = GuardState::load(&dir.path().join("guard").join(STATE_FILE));
    assert_eq!(back.lifecycle, promoted);
}

/// The boot's live entry is an entry like any other: a failure unwinds to
/// REAPER. A maintenance build it would have made the pin is not (the
/// review of lane 1): the lifecycle changes only once the PC is live.
#[test]
fn in_prod_a_failed_boot_into_live_unwinds_to_reaper() {
    let dir = tempfile::tempdir().unwrap();
    let st = GuardState {
        lifecycle: prod_on(None, Some(NEW)),
        ..GuardState::default()
    };
    let mut g = saved(dir.path(), st, 1_000);
    let mut pc = FakePc::new(Facts::default());
    pc.fail(Call::EngineStart, "the engine did not start");
    assert_eq!(start(&mut pc, &mut g, 5_000), Some(Outcome::Done));
    assert_eq!(g.state.mode, Mode::Event);
    assert!(pc.called(Call::ReaperStart) && pc.called(Call::AppStart));
    assert_eq!(pc.bundle.as_deref(), Some(NEW), "it tried the new pin");
    assert_eq!(g.state.lifecycle, prod_on(None, Some(NEW)));
}

/// G8 at the boot: a pin that is no green main build goes nowhere live;
/// the start's checks run in event and the alarm names it.
#[test]
fn in_prod_a_reboot_on_a_red_pin_stays_in_event_with_an_alarm() {
    let dir = tempfile::tempdir().unwrap();
    let mut st = GuardState {
        lifecycle: prod_on(None, None),
        ..GuardState::default()
    };
    st.bundles.insert(SHA.into(), record(SHA, "main", Hil::Red));
    let gdir = dir.path().join("guard");
    std::fs::create_dir_all(&gdir).unwrap();
    st.save(&gdir.join(STATE_FILE), 1_000).unwrap();
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    let mut pc = FakePc::new(band_up());
    assert_eq!(start(&mut pc, &mut g, 5_000), Some(Outcome::Done));
    assert_eq!(g.state.mode, Mode::Event);
    assert!(!pc.called(Call::EngineStart) && !pc.called(Call::ReaperSaveQuit));
    let alarm =
        format!("after a reboot in prod: {SHA}: HIL Red; live needs green; the PC stays in event");
    assert_eq!(texts(&g).first(), Some(&alarm));
    assert_eq!(g.state.lifecycle, prod_on(None, None));
}

/// A prod live entry that fails keeps the pin it would have ended
/// maintenance on (the review of lane 1).
#[test]
fn in_prod_a_failed_live_entry_keeps_the_pin_and_the_session() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    g.state.lifecycle = prod_on(None, Some(NEW));
    g.state.active = Some(NEW.into());
    for sha in [SHA, NEW] {
        g.state
            .bundles
            .insert(sha.into(), record(sha, "main", Hil::Green));
    }
    pc.fail(Call::EngineStart, "the engine did not start");
    let live = Request::Live {
        build: Some(NEW.into()),
        trial: false,
        dry_run: false,
    };
    let r = handle(&mut pc, &mut g, live, INIT);
    assert!(!r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Event);
    assert_eq!(g.state.lifecycle, prod_on(None, Some(NEW)));
    assert!(!r.detail.contains("becomes the pin"), "{}", r.detail);
}

/// The Trial exclusions (the review of lane 1): HIL's `dev --build B` then
/// `activate B` keeps the way back A's Defender exclusions, as before S8.
#[test]
fn an_entry_then_an_activation_of_its_build_keeps_the_way_back_s_exclusions() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    let zip = install::tests::good_zip(dir.path(), SHA);
    assert!(install_bundle(&mut g, &zip).0);
    g.state.pins = other_pinned();
    let mut pc = FakePc::new(Facts::default());
    let dev = Request::Dev {
        build: Some(SHA.into()),
        dry_run: false,
    };
    assert!(handle(&mut pc, &mut g, dev, INIT).ok);
    // Sent after the entry's reply: the generation of that moment.
    let seen = g.shared.generation();
    let r = handle(&mut pc, &mut g, Request::Activate { sha: SHA.into() }, seen);
    assert!(r.ok, "{r:?}");
    assert_eq!(pc.excluded, [(SHA.to_owned(), vec![OTHER.to_owned()])]);
    assert_eq!(g.state.way_back_bundle(), Some(OTHER));
    assert_eq!(g.state.pins, mirrored(SHA, Some(OTHER)));
}

/// In prod an "ide event" stands across a guard restart: the band's system
/// up without our engine is event, never live.
#[test]
fn in_prod_a_guard_restart_with_the_band_up_stays_in_event() {
    let dir = tempfile::tempdir().unwrap();
    let st = GuardState {
        lifecycle: prod_on(None, None),
        active: Some(SHA.into()),
        ..GuardState::default()
    };
    let mut g = saved(dir.path(), st, 9_000);
    let mut pc = FakePc::new(band_up());
    assert_eq!(start(&mut pc, &mut g, 5_000), Some(Outcome::Done));
    assert_eq!(g.state.mode, Mode::Event);
    assert!(!pc.called(Call::EngineStart) && !pc.called(Call::ReaperSaveQuit));
    // Live with our engine up, booted the second the state was written: a
    // guard restart, no reboot; live stays as it is.
    let st = GuardState {
        mode: Mode::Live,
        lifecycle: prod_on(None, None),
        active: Some(SHA.into()),
        ..GuardState::default()
    };
    let mut g = saved(dir.path(), st, 9_000);
    let mut pc = FakePc::new(iemmixer_up());
    assert_eq!(start(&mut pc, &mut g, 9_000), None);
    assert_eq!(g.state.mode, Mode::Live);
    assert!(!pc.called(Call::EngineStart) && !pc.called(Call::EngineStop));
}

/// A rollback goes on to event at a start, also without a reboot, and no
/// entry runs while it does.
#[test]
fn a_start_while_rolling_back_goes_to_event() {
    let dir = tempfile::tempdir().unwrap();
    let st = GuardState {
        mode: Mode::Live,
        lifecycle: Lifecycle::RollingBack,
        active: Some(SHA.into()),
        ..GuardState::default()
    };
    let mut g = saved(dir.path(), st, 9_000);
    let mut pc = FakePc::new(iemmixer_up());
    assert_eq!(start(&mut pc, &mut g, 5_000), Some(Outcome::Done));
    assert_eq!(g.state.mode, Mode::Event);
    assert!(pc.called(Call::EngineStop) && pc.called(Call::ReaperStart));
    assert_eq!(g.state.lifecycle, Lifecycle::RollingBack);
    let dev = Request::Dev {
        build: None,
        dry_run: false,
    };
    let seen = g.shared.generation();
    let r = handle(&mut pc, &mut g, dev, seen);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, crate::lifecycle::ROLLING_BACK)
    );
}

/// Maintenance (design §3.4): `dev --build NEW` runs NEW and the pin stays;
/// the next live entry makes NEW (green, main) the pin and runs it. A trial
/// and a live entry on another build are refused, nothing runs.
#[test]
fn in_prod_maintenance_ends_with_its_green_build_as_the_pin() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Live));
    g.state.lifecycle = prod_on(Some(OTHER), None);
    g.state.active = Some(SHA.into());
    for sha in [SHA, OTHER, NEW] {
        g.state
            .bundles
            .insert(sha.into(), record(sha, "main", Hil::Green));
    }
    let live = |build: &str, trial| Request::Live {
        build: Some(build.into()),
        trial,
        dry_run: false,
    };
    let made = pc.calls().len();
    for (req, why) in [
        (
            live(SHA, true),
            format!("after the cutover there are no trials: live runs the pin {SHA}"),
        ),
        (
            live(NEW, false),
            format!("in prod live runs the pin {SHA}: live --build {SHA}"),
        ),
    ] {
        let seen = g.shared.generation();
        let r = handle(&mut pc, &mut g, req, seen);
        assert_eq!((r.ok, r.detail), (false, why));
    }
    assert_eq!(pc.calls().len(), made, "nothing ran");
    let dev = Request::Dev {
        build: Some(NEW.into()),
        dry_run: false,
    };
    let seen = g.shared.generation();
    assert!(handle(&mut pc, &mut g, dev, seen).ok);
    assert_eq!(g.state.mode, Mode::Dev);
    assert_eq!(pc.bundle.as_deref(), Some(NEW));
    assert_eq!(g.state.lifecycle, prod_on(Some(OTHER), Some(NEW)));
    assert!(status_text(&g).contains(&format!("maintenance {NEW}")));
    let note = format!("maintenance build {NEW} becomes the pin; {SHA} is the previous pin");
    // The dry run says what the entry would decide, and changes nothing.
    let dry = Request::Live {
        build: Some(NEW.into()),
        trial: false,
        dry_run: true,
    };
    let seen = g.shared.generation();
    let r = handle(&mut pc, &mut g, dry, seen);
    assert!(r.ok, "{r:?}");
    assert!(r.detail.ends_with(&format!("; {note}")), "{}", r.detail);
    assert_eq!(g.state.lifecycle, prod_on(Some(OTHER), Some(NEW)));
    let seen = g.shared.generation();
    let r = handle(&mut pc, &mut g, live(NEW, false), seen);
    assert!(r.ok, "{r:?}");
    assert!(r.detail.contains(&note), "{}", r.detail);
    assert_eq!(g.state.mode, Mode::Live);
    assert_eq!(pc.bundle.as_deref(), Some(NEW));
    assert_eq!(
        g.state.lifecycle,
        Lifecycle::Prod(Prod {
            since: T0,
            pin: NEW.into(),
            previous: Some(SHA.into()),
            maintenance: None,
        })
    );
    // The legacy record mirrors the active bundle and the way back, never
    // the pin: an older guard taking over runs NEW.
    assert_eq!(g.state.pins, mirrored(NEW, Some(SHA)));
}

/// A crash loop in maintenance: iemmixer stops and live runs on the pin;
/// the session's build never becomes the pin, and the pin's engine starts
/// a new crash window.
#[test]
fn a_crash_loop_in_maintenance_goes_live_on_the_pin() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Dev));
    g.state.lifecycle = prod_on(None, Some(NEW));
    g.state.active = Some(NEW.into());
    green_main(&mut g, &[SHA, NEW]);
    pc.exited = vec![(Kid::Engine, Some(70)); 3];
    let at = Instant::now();
    tick(&mut pc, &mut g, at);
    assert_eq!(g.state.mode, Mode::Live);
    assert_eq!(g.state.lifecycle, prod_on(None, None));
    assert_eq!(g.state.active_bundle(), Some(SHA));
    assert_eq!(pc.bundle.as_deref(), Some(SHA));
    // The live entry's engine only: no respawn of the session's one.
    tick(&mut pc, &mut g, at + Duration::from_secs(30));
    assert_eq!(pc.count(Call::EngineStart), 1);
    assert_eq!(
        texts(&g),
        [format!(
            "the engine crashed 3 times in 10 min in maintenance: back to live on the pin {SHA}"
        )]
    );
    assert_eq!(g.crash.in_window(), 0);
}

/// Prod live: a crash loop reverts to the previous pin; when the previous
/// pin loops too (its own three exits), the engine stays down and the alarm
/// names the rollback.
#[test]
fn in_prod_the_previous_pin_that_loops_too_stays_down() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Live));
    g.state.lifecycle = prod_on(Some(OTHER), None);
    green_main(&mut g, &[SHA, OTHER]);
    let at = Instant::now();
    pc.exited = vec![(Kid::Engine, Some(70)); 3];
    tick(&mut pc, &mut g, at);
    assert_eq!(pc.count(Call::EngineStart), 1);
    // One exit of the previous pin's engine is no loop: it respawns.
    pc.exited = vec![(Kid::Engine, Some(70))];
    tick(&mut pc, &mut g, at + Duration::from_secs(5));
    tick(&mut pc, &mut g, at + Duration::from_secs(7));
    assert_eq!(pc.count(Call::EngineStart), 2);
    pc.exited = vec![(Kid::Engine, Some(70)); 2];
    tick(&mut pc, &mut g, at + Duration::from_secs(10));
    let down = format!(
        "the engine crashed 3 times in 10 min on the pin {OTHER}, and no previous pin is left: \
         the engine stays down; roll back to REAPER (iemmode rollback)"
    );
    assert_eq!(texts(&g).last(), Some(&down));
    tick(&mut pc, &mut g, at + Duration::from_secs(60));
    assert_eq!(pc.count(Call::EngineStart), 2, "no respawn");
    assert_eq!(g.state.mode, Mode::Live);
}

/// An activation in prod keeps the exclusions of the bundle active before
/// it and of both pins, and leaves the pins.
#[test]
fn an_activation_in_prod_keeps_the_pins_exclusions() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    g.state.mode = Mode::Dev;
    g.state.lifecycle = prod_on(Some(OTHER), None);
    g.state.active = Some(SHA.into());
    let zip = install::tests::good_zip(dir.path(), NEW);
    assert!(install_bundle(&mut g, &zip).0);
    let mut pc = FakePc::new(iemmixer_up());
    let r = handle(&mut pc, &mut g, Request::Activate { sha: NEW.into() }, INIT);
    assert!(r.ok, "{r:?}");
    assert_eq!(
        pc.excluded,
        [(NEW.to_owned(), vec![SHA.to_owned(), OTHER.to_owned()])]
    );
    assert_eq!(g.state.active_bundle(), Some(NEW));
    assert_eq!(g.state.lifecycle, prod_on(Some(OTHER), None));
    let status = status_text(&g);
    let want = format!("bundle {NEW}; prod since {T0}: pin {SHA}, previous {OTHER}");
    assert!(status.contains(&want), "{status}");
}

/// S8 lane 2: in prod `live` needs no build; it runs the pin. Before the
/// cutover it still needs one (a trial).
#[test]
fn in_prod_live_without_a_build_runs_the_pin() {
    let live = Request::Live {
        build: None,
        trial: false,
        dry_run: false,
    };
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    g.state.lifecycle = prod_on(None, None);
    g.state.active = Some(OTHER.into());
    green_main(&mut g, &[SHA, OTHER]);
    let r = handle(&mut pc, &mut g, live.clone(), INIT);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Live);
    assert_eq!(pc.bundle.as_deref(), Some(SHA));
    assert_eq!(g.state.active_bundle(), Some(SHA));
    assert_eq!(g.state.lifecycle, prod_on(None, None));
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    green_main(&mut g, &[SHA]);
    let r = handle(&mut pc, &mut g, live, INIT);
    assert_eq!((r.ok, r.detail.as_str()), (false, "live needs --build SHA"));
    assert_eq!(g.state.mode, Mode::Dev);
}

/// The prod data rule (ROZHODNUTÉ on #11; program spec §3: the REAPER
/// project is the data authority only before the cutover): in prod no
/// entry refreshes the band's data from the predecessor. The boot's live
/// entry on the pin, a maintenance dev entry and the live entry that ends
/// it run no data step, and the dry run lists none; iemmixer's own state is
/// the only authority. A trial still imports.
#[test]
fn in_prod_no_entry_refreshes_the_data_from_the_predecessor() {
    let dir = tempfile::tempdir().unwrap();
    let st = GuardState {
        lifecycle: prod_on(None, None),
        ..GuardState::default()
    };
    let mut g = saved(dir.path(), st, 1_000);
    let mut pc = FakePc::new(Facts::default());
    assert_eq!(start(&mut pc, &mut g, 5_000), Some(Outcome::Done));
    assert_eq!(g.state.mode, Mode::Live);
    assert!(
        !pc.called(Call::Data),
        "the boot's live entry refreshed the data"
    );
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Live));
    g.state.lifecycle = prod_on(None, None);
    g.state.active = Some(SHA.into());
    green_main(&mut g, &[SHA, NEW]);
    let maintenance = Request::Dev {
        build: Some(NEW.into()),
        dry_run: false,
    };
    let r = handle(&mut pc, &mut g, maintenance, INIT);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Dev);
    let live = |dry_run| Request::Live {
        build: None,
        trial: false,
        dry_run,
    };
    let r = ask(&mut pc, &mut g, live(true));
    assert!(r.ok, "{r:?}");
    assert!(!r.detail.contains("Data"), "{}", r.detail);
    let r = ask(&mut pc, &mut g, live(false));
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Live);
    assert!(!pc.called(Call::Data), "a prod entry refreshed the data");
    // Before the cutover a trial imports the band's REAPER mix, as before.
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    green_main(&mut g, &[SHA]);
    let trial = Request::Live {
        build: Some(SHA.into()),
        trial: true,
        dry_run: false,
    };
    assert!(handle(&mut pc, &mut g, trial, INIT).ok);
    assert_eq!(pc.count(Call::Data), 1);
}
