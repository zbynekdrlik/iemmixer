//! Activations and site installs against `FakePc` (`daemon/activation.rs`):
//! inside a HIL job, in an idle event, refused in live, without a guard, and
//! install-site with its restarts.

use std::time::{Duration, Instant};

use super::tests::{INIT, OTHER, SHA, T0, ask, band_up, fixed, iemmixer_up, record, steps, texts};
use super::*;
use crate::bundle::Hil;
use crate::install;
use crate::pc::fake::{Call, FakePc};
use crate::plan::Facts;
use crate::proto::{GUARD_BUILD, Request};
use crate::state::Switching;

#[test]
fn activate_in_a_job_restarts_the_engine_and_the_server() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    g.state.mode = Mode::Dev;
    g.state.job = Some(7);
    let mut pc = FakePc::new(Facts {
        runner: true,
        ..iemmixer_up()
    });
    let zip = install::tests::good_zip(dir.path(), SHA);
    assert!(install_bundle(&mut g, &zip).0);
    // The pin before this one keeps its Defender exclusions.
    g.state.pins.current = Some(OTHER.into());
    let r = handle(&mut pc, &mut g, Request::Activate { sha: SHA.into() }, INIT);
    assert_eq!(pc.excluded, [(SHA.to_owned(), vec![OTHER.to_owned()])]);
    assert_eq!(
        (r.ok, r.detail.clone()),
        (
            true,
            format!(
                "activated {SHA}; the engine and the server run it; the guard hands over to its \
                 new exe; engine started, held, with its HIL flags (pid 1001); engine ready: 32 \
                 frames, 30000 callbacks, 0 missed; server started (pid 1002)"
            )
        )
    );
    assert_eq!(
        steps(&pc)
            .into_iter()
            .filter(|c| c.mutates() || *c == Call::EngineReady)
            .collect::<Vec<_>>(),
        [
            Call::Exclude,
            Call::EngineStop,
            Call::ServerStop,
            Call::PrefCheck,
            Call::EngineStart,
            Call::EngineReady,
            Call::EngineArm,
            Call::ServerStart,
        ]
    );
    assert_eq!(pc.engine_starts, [(true, true)]);
    assert_eq!(g.state.job, Some(7), "the job goes on");
    // The job is in the state: the guard the activation hands over to serves it.
    let back = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    assert_eq!(back.state.job, Some(7));
    // Outside a job the engine is not touched.
    let mut pc = FakePc::new(iemmixer_up());
    g.state.job = None;
    let r = handle(&mut pc, &mut g, Request::Activate { sha: SHA.into() }, INIT);
    assert_eq!((r.ok, r.detail.clone()), (true, format!("activated {SHA}")));
    assert!(!pc.called(Call::EngineStop) && !pc.called(Call::EngineStart));
}

/// A guard with bundle `SHA` installed (its files under `dir`), in event,
/// `OTHER` pinned (the running guard's bundle).
fn installed_in_event(dir: &Path) -> Guard {
    let mut g = Guard::open(dir, SiteConf::default(), fixed(T0));
    assert_eq!(g.state.mode, Mode::Event);
    let zip = install::tests::good_zip(dir, SHA);
    assert!(install_bundle(&mut g, &zip).0);
    g.state.pins.current = Some(OTHER.into());
    g
}

/// Every call to REAPER or the predecessor app, reads included.
const BAND_CALLS: [Call; 7] = [
    Call::ReaperSaveQuit,
    Call::AppStop,
    Call::HolderGone,
    Call::ReaperStart,
    Call::ReaperFacts,
    Call::AppStart,
    Call::AppAnswers,
];

/// A guard fix reaches a guard in event (#9 2026-09-28): a guard that
/// refuses the dev entry could never enter dev to take it. In an idle
/// event `activate` copies the bins, pins, sets the exclusions and hands
/// over, with no call to REAPER or the app. The new exe's guard starts as
/// after any guard restart: in event its start runs the event plan's
/// checks, which read REAPER and the app and neither stop nor start them.
#[test]
fn activate_in_an_idle_event_hands_over_and_leaves_reaper_and_the_app_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = installed_in_event(dir.path());
    let mut pc = FakePc::new(band_up());
    let r = handle(&mut pc, &mut g, Request::Activate { sha: SHA.into() }, INIT);
    assert_eq!(
        (r.ok, r.detail.clone()),
        (
            true,
            format!("activated {SHA}; the guard hands over to its new exe")
        )
    );
    assert_eq!(pc.calls(), [Call::Facts, Call::SetBundle, Call::Exclude]);
    for c in BAND_CALLS {
        assert!(!pc.called(c), "{c:?}");
    }
    assert_eq!(pc.excluded, [(SHA.to_owned(), vec![OTHER.to_owned()])]);
    assert_eq!(pc.bundle.as_deref(), Some(SHA));
    assert_eq!(
        (g.state.mode, g.state.pins.current.as_deref()),
        (Mode::Event, Some(SHA))
    );
    assert_eq!(
        g.handover,
        Some(install::bin_dir(dir.path()).join(install::GUARD_EXE))
    );
    assert!(g.alarms.all().is_empty(), "{:?}", texts(&g));
    assert_eq!(r.guard_build.as_deref(), Some(GUARD_BUILD));
    // The new exe's guard: the pin it finds, event kept, REAPER and the
    // app neither stopped nor started.
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    let mut pc = FakePc::new(band_up());
    assert_eq!(start(&mut pc, &mut g, 0), Some(Outcome::Done));
    assert_eq!(
        (g.state.mode, g.state.pins.current.as_deref()),
        (Mode::Event, Some(SHA))
    );
    assert_eq!(pc.bundle.as_deref(), Some(SHA));
    for c in [
        Call::ReaperSaveQuit,
        Call::AppStop,
        Call::ReaperStart,
        Call::AppStart,
        Call::EngineStart,
    ] {
        assert!(!pc.called(c), "{c:?}");
    }
}

#[test]
fn activate_in_event_is_refused_while_the_guard_is_not_idle() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = installed_in_event(dir.path());
    let bin_guard = install::bin_dir(dir.path()).join(install::GUARD_EXE);
    let activate = || Request::Activate { sha: SHA.into() };
    // A process of iemmixer's runs.
    let mut pc = FakePc::new(Facts {
        engine: true,
        tray: true,
        ..band_up()
    });
    let r = handle(&mut pc, &mut g, activate(), INIT);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (
            false,
            "activate in event needs no iemmixer process; running: engine, tray"
        )
    );
    assert!(steps(&pc).is_empty(), "{:?}", pc.calls());
    let mut pc = FakePc::new(band_up());
    // A switch persisted as unfinished.
    g.state.switching = Some(Switching {
        from: Mode::Event,
        to: Mode::Dev,
        done: vec![Step::Precheck],
        started: T0,
    });
    let r = handle(&mut pc, &mut g, activate(), INIT);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "a switch is in progress: activate waits for its end")
    );
    g.state.switching = None;
    // A HIL job that did not end.
    g.state.job = Some(7);
    let r = handle(&mut pc, &mut g, activate(), INIT);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "HIL job 7 runs: activate waits for its end")
    );
    // Nothing was copied, pinned, excluded or handed over.
    assert!(steps(&pc).is_empty(), "{:?}", pc.calls());
    assert!(!bin_guard.exists());
    assert_eq!(
        (g.state.pins.current.as_deref(), g.handover.as_ref()),
        (Some(OTHER), None)
    );
    // Idle again: a bundle that is not installed is named.
    g.state.job = None;
    let missing = "f".repeat(40);
    let r = handle(
        &mut pc,
        &mut g,
        Request::Activate {
            sha: missing.clone(),
        },
        INIT,
    );
    assert_eq!(
        (r.ok, r.detail),
        (false, format!("bundle {missing} is not installed"))
    );
    assert!(steps(&pc).is_empty(), "{:?}", pc.calls());
}

/// Live activates its bundle through `live --build`.
#[test]
fn activate_in_live_is_refused() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Live));
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Green));
    let r = handle(&mut pc, &mut g, Request::Activate { sha: SHA.into() }, INIT);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (
            false,
            "activate is for dev and an idle event; the mode is live (live --build activates its \
             bundle)"
        )
    );
    assert!(steps(&pc).is_empty(), "{:?}", pc.calls());
    assert!(pc.excluded.is_empty());
    assert_eq!(g.state.pins.current, None);
}

/// Every reply names the build of the guard that answered, so a hand-over
/// to a new exe is verifiable (`iempc activate` waits for it).
#[test]
fn every_reply_names_the_guard_s_build() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    let r = ask(&mut pc, &mut g, Request::Status);
    assert_eq!(r.guard_build.as_deref(), Some(GUARD_BUILD));
    let r = ask(&mut pc, &mut g, Request::ProbeTask);
    assert_eq!(r.guard_build.as_deref(), Some(GUARD_BUILD));
    // The pipe answers a status (and a refusal while switching) from the view.
    match g.shared.route(&Request::Status) {
        Route::Now(r) => assert_eq!(r.guard_build.as_deref(), Some(GUARD_BUILD)),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        g.shared
            .view()
            .reply(false, "switching")
            .guard_build
            .as_deref(),
        Some(GUARD_BUILD)
    );
}

/// The offline refusal of a saved mode other than event.
fn offline_mode_refusal(mode: &str) -> String {
    format!("the saved mode is {mode}: without a guard only an idle event activates")
}

/// `iemmixer-guard activate <sha>` while no guard runs (#9 2026-09-28): the
/// way to a guard too old to activate in event (its own code refuses it).
/// Under the guard's mutex, the same rule on the saved state and the
/// process list; then the bins, the pin and the exclusions, saved. It
/// starts no guard: the next `iemmode` call starts the guard's task, which
/// runs the new exe from `bin\`.
#[test]
fn activate_offline_in_an_idle_event_copies_pins_and_starts_no_guard() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = installed_in_event(dir.path());
    let mut pc = FakePc::new(band_up());
    let r = activate_offline(&mut pc, &mut g, Some(()), SHA);
    assert_eq!(
        (r.ok, r.detail.clone()),
        (
            true,
            format!(
                "activated {SHA} without a guard; the next iemmode call starts the guard from bin"
            )
        )
    );
    assert_eq!(pc.calls(), [Call::Facts, Call::SetBundle, Call::Exclude]);
    assert_eq!(pc.excluded, [(SHA.to_owned(), vec![OTHER.to_owned()])]);
    let bin = install::bin_dir(dir.path());
    assert!(bin.join(install::GUARD_EXE).is_file());
    assert!(bin.join(install::IEMMODE_EXE).is_file());
    assert_eq!(g.handover, None, "no guard is started or handed over to");
    assert_eq!(
        (r.mode, r.guard_build.as_deref()),
        (Mode::Event, Some(GUARD_BUILD))
    );
    // Saved: the guard the next iemmode call starts finds the pin.
    let back = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    assert_eq!(
        (back.state.mode, back.state.pins.current.as_deref()),
        (Mode::Event, Some(SHA))
    );
    // Exclusions that fail alarm (the alarm is kept for the next guard);
    // the activation stands.
    let dir = tempfile::tempdir().unwrap();
    let mut g = installed_in_event(dir.path());
    let mut pc = FakePc::new(band_up());
    pc.fail(Call::Exclude, "the exclude task ended with 1");
    let r = activate_offline(&mut pc, &mut g, Some(()), SHA);
    assert!(r.ok, "{r:?}");
    let back = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    assert_eq!(
        texts(&back),
        [format!(
            "Defender exclusions for {SHA}: the exclude task ended with 1"
        )]
    );
    assert_eq!(back.state.pins.current.as_deref(), Some(SHA));
}

#[test]
fn activate_offline_is_refused_while_a_guard_runs_or_the_event_is_not_idle() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = installed_in_event(dir.path());
    let bin_guard = install::bin_dir(dir.path()).join(install::GUARD_EXE);
    // A guard holds the mutex: nothing is read.
    let mut pc = FakePc::new(band_up());
    let r = activate_offline::<()>(&mut pc, &mut g, None, SHA);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "a guard runs; use iemmode activate")
    );
    assert!(pc.calls().is_empty(), "{:?}", pc.calls());
    // Dev and live by the saved mode: a guard serves them.
    for (mode, name) in [(Mode::Dev, "dev"), (Mode::Live, "live")] {
        g.state.mode = mode;
        let r = activate_offline(&mut pc, &mut g, Some(()), SHA);
        assert_eq!((r.ok, r.detail), (false, offline_mode_refusal(name)));
    }
    assert!(pc.calls().is_empty(), "{:?}", pc.calls());
    g.state.mode = Mode::Event;
    // A process of iemmixer's runs.
    let mut pc = FakePc::new(Facts {
        engine: true,
        ..band_up()
    });
    let r = activate_offline(&mut pc, &mut g, Some(()), SHA);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (
            false,
            "activate in event needs no iemmixer process; running: engine"
        )
    );
    let mut pc = FakePc::new(band_up());
    // A bundle that is not installed.
    let r = activate_offline(&mut pc, &mut g, Some(()), OTHER);
    assert_eq!(
        (r.ok, r.detail),
        (false, format!("bundle {OTHER} is not installed"))
    );
    // Nothing was copied, pinned or excluded.
    assert!(steps(&pc).is_empty(), "{:?}", pc.calls());
    assert!(!bin_guard.exists());
    assert_eq!(g.state.pins.current.as_deref(), Some(OTHER));
}

#[test]
fn install_site_in_a_job_restarts_only_the_engine_and_the_server() {
    let (mut pc, mut g) = (
        FakePc::new(Facts {
            runner: true,
            ..iemmixer_up()
        }),
        Guard::for_test(Mode::Dev),
    );
    g.state.pins.current = Some(SHA.into());
    g.state.job = Some(7);
    let site = Request::InstallSite {
        path: "site.toml".into(),
    };
    let r = handle(&mut pc, &mut g, site.clone(), INIT);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (
            true,
            "site installed; the engine and the server run it (HIL job); site.toml: checked and installed; engine started, held, with its HIL flags (pid 1001); engine ready: 32 frames, 30000 callbacks, 0 missed; server started (pid 1002)"
        )
    );
    for c in [
        Call::RunnerStop,
        Call::TrayStop,
        Call::Data,
        Call::Tuning,
        Call::RunnerStart,
    ] {
        assert!(!pc.called(c), "{c:?}: no dev entry inside a job");
    }
    assert_eq!(pc.engine_starts, [(true, true)]);
    assert_eq!(g.state.job, Some(7));
    // The children are saved after the last step too (a guard that takes
    // over adopts the new engine and server).
    assert_eq!(pc.calls_after(Call::ServerStart), [Call::Children]);
    // A failed start unwinds to event, as a failed dev entry does.
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    g.state.pins.current = Some(SHA.into());
    g.state.job = Some(7);
    pc.fail(Call::ServerStart, "ports 80/443 are still held");
    let r = handle(&mut pc, &mut g, site, INIT);
    assert!(!r.ok);
    assert!(
        r.detail
            .starts_with("site installed; ServerStart: ports 80/443 are still held; event: done"),
        "{}",
        r.detail
    );
    assert_eq!(g.state.mode, Mode::Event);
    assert!(pc.called(Call::ReaperStart));
    assert_eq!(texts(&g)[0], "ServerStart: ports 80/443 are still held");
    assert_eq!(g.state.job, None, "no HIL job outside dev");
}

/// Inside a HIL job the engine starts without a plan (`restart_in_job`,
/// for activate and install-site): an engine that ended holding the card
/// left 32, and the new one refuses the card unless it finds REAPER's
/// original (exit 3, #9 2026-09-28). So the same check runs right before
/// the start, after the old engine stopped. A failed restore starts
/// nothing, alarms and unwinds to event, as any failed step there.
#[test]
fn a_job_restart_restores_the_preference_right_before_the_engine_starts() {
    let (mut pc, mut g) = (
        FakePc::new(Facts {
            runner: true,
            ..iemmixer_up()
        }),
        Guard::for_test(Mode::Dev),
    );
    g.state.pins.current = Some(SHA.into());
    g.state.job = Some(7);
    pc.pref_attempts = 1;
    let site = Request::InstallSite {
        path: "site.toml".into(),
    };
    let r = handle(&mut pc, &mut g, site.clone(), INIT);
    assert!(r.ok, "{r:?}");
    assert!(
        r.detail
            .contains("; the preferred buffer was restored (1 writes); engine started"),
        "{}",
        r.detail
    );
    assert_eq!(
        steps(&pc)
            .into_iter()
            .filter(|c| c.mutates())
            .collect::<Vec<_>>(),
        [
            Call::InstallSite,
            Call::EngineStop,
            Call::ServerStop,
            Call::PrefCheck,
            Call::EngineStart,
            Call::EngineArm,
            Call::ServerStart,
        ]
    );
    assert_eq!(pc.pref_writes, 1);
    assert_eq!(pc.engine_starts, [(true, true)]);
    assert_eq!(g.state.job, Some(7));

    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    g.state.pins.current = Some(SHA.into());
    g.state.job = Some(7);
    pc.fail(
        Call::PrefCheck,
        "writing the preferred buffer failed: access denied",
    );
    let r = handle(&mut pc, &mut g, site, INIT);
    assert!(!r.ok);
    assert!(
        r.detail.starts_with(
            "site installed; PrefCheck: writing the preferred buffer failed: access denied; \
             event: done"
        ),
        "{}",
        r.detail
    );
    assert!(!pc.called(Call::EngineStart));
    assert_eq!(
        texts(&g)[0],
        "PrefCheck: writing the preferred buffer failed: access denied"
    );
    assert_eq!(g.state.mode, Mode::Event);
    assert!(pc.called(Call::ReaperStart));
    assert_eq!(g.state.job, None);
}

#[test]
fn install_site_checks_the_site_and_enters_dev_again() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    g.state.pins.current = Some(SHA.into());
    pc.fail(Call::InstallSite, "check-site ended with Some(2)");
    let r = handle(
        &mut pc,
        &mut g,
        Request::InstallSite {
            path: "bad.toml".into(),
        },
        INIT,
    );
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "site refused: check-site ended with Some(2)")
    );
    assert!(!pc.called(Call::EngineStop));
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    g.state.pins.current = Some(SHA.into());
    let r = handle(
        &mut pc,
        &mut g,
        Request::InstallSite {
            path: "site.toml".into(),
        },
        INIT,
    );
    assert!(r.ok, "{r:?}");
    assert!(
        r.detail
            .starts_with("site installed; dev: done; site.toml: checked and installed"),
        "{}",
        r.detail
    );
    assert!(pc.index(Call::InstallSite) < pc.index(Call::EngineStop));
    assert!(pc.index(Call::EngineStop) < pc.index(Call::EngineStart));
    assert!(pc.called(Call::ServerStart));
    assert!(!pc.called(Call::ReaperStart));
    assert_eq!(g.state.mode, Mode::Dev);
}

#[test]
fn an_install_site_whose_dev_entry_unwinds_is_no_success() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    g.state.pins.current = Some(SHA.into());
    pc.fail(Call::ServerStart, "ports 80/443 are still held");
    let r = handle(
        &mut pc,
        &mut g,
        Request::InstallSite {
            path: "site.toml".into(),
        },
        INIT,
    );
    assert!(!r.ok);
    assert!(
        r.detail
            .starts_with("site installed; dev: not entered; unwound to event"),
        "{}",
        r.detail
    );
    assert_eq!(pc.sites, ["site.toml"]);
    assert_eq!(g.state.mode, Mode::Event);
    assert!(pc.index(Call::ReaperStart) > pc.index(Call::ServerStart));
}

#[test]
fn ide_event_ends_the_site_check_at_once() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    g.state.pins.current = Some(SHA.into());
    pc.block_until_cancel(Call::InstallSite);
    let shared = Arc::clone(&g.shared);
    let fired = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        // No switch runs: "ide event" pre-empts the token and queues.
        let route = shared.route(&Request::Event { dry_run: false });
        (route, Instant::now())
    });
    let r = handle(
        &mut pc,
        &mut g,
        Request::InstallSite {
            path: "site.toml".into(),
        },
        INIT,
    );
    let (route, at) = fired.join().unwrap();
    // Its own generation holds the fence it moved (#42).
    assert_eq!(route, Route::Queue(Generation { epoch: 0, fence: 1 }));
    assert!(at.elapsed() < Duration::from_secs(1), "{:?}", at.elapsed());
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "site refused: pre-empted by event")
    );
    assert!(pc.sites.is_empty());
    assert!(!pc.called(Call::EngineStop));
}
