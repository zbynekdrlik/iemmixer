//! The start after a reboot or a restart against `FakePc`
//! (`daemon/startup.rs`), the start's checks with a dev entry queued (#42),
//! `--direct`, and the files: the state and alarms, bundle install and
//! activation.

use std::time::{Duration, Instant};

use super::tests::{INIT, OTHER, SHA, T0, ask, band_up, dev, fixed, iemmixer_up, record, texts};
use super::*;
use crate::bundle::Hil;
use crate::install;
use crate::pc::fake::{Call, FakePc};
use crate::plan::Facts;
use crate::proto::Request;
use crate::state::Switching;

fn save_state(root: &Path, st: &mut GuardState, at: u64) {
    let dir = root.join("guard");
    std::fs::create_dir_all(&dir).unwrap();
    st.save(&dir.join(STATE_FILE), at).unwrap();
}

#[test]
fn a_reboot_resets_the_mode_to_event() {
    let dir = tempfile::tempdir().unwrap();
    let mut saved = GuardState {
        mode: Mode::Dev,
        switching: Some(Switching {
            from: Mode::Event,
            to: Mode::Dev,
            done: vec![Step::Precheck],
            started: 1_000,
        }),
        ..GuardState::default()
    };
    save_state(dir.path(), &mut saved, 1_000);
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    assert_eq!(g.state.mode, Mode::Dev);
    // After the reboot REAPER and the app came up by themselves.
    let mut pc = FakePc::new(band_up());
    assert_eq!(start(&mut pc, &mut g, 5_000), Some(Outcome::Done));
    assert_eq!(g.state.mode, Mode::Event);
    assert_eq!(g.state.switching, None);
    for c in [
        Call::Tuning,
        Call::PrefCheck,
        Call::ReaperFacts,
        Call::AppAnswers,
        Call::Fingerprint,
    ] {
        assert!(pc.called(c), "{c:?}: the event plan's checks");
    }
    assert!(!pc.called(Call::ReaperStart) && !pc.called(Call::EngineStop));
    let (back, err) = GuardState::load(&dir.path().join("guard").join(STATE_FILE));
    assert_eq!(err, None);
    assert_eq!((back.mode, back.switching), (Mode::Event, None));
    assert_eq!(back.written_at, T0);
    // A guard restart (no reboot) with the engine running keeps dev.
    let mut saved = GuardState {
        mode: Mode::Dev,
        ..GuardState::default()
    };
    save_state(dir.path(), &mut saved, 9_000);
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    let mut pc = FakePc::new(iemmixer_up());
    assert_eq!(start(&mut pc, &mut g, 5_000), None);
    assert_eq!(g.state.mode, Mode::Dev);
    assert!(!pc.called(Call::EngineStop));
    assert!(pc.called(Call::Adopt) && pc.called(Call::SetBundle));
    // REAPER running beside our engine (a deployment of the predecessor)
    // is no reboot either: dev stays, the watch alarms.
    let mut saved = GuardState {
        mode: Mode::Dev,
        ..GuardState::default()
    };
    save_state(dir.path(), &mut saved, 9_000);
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    let mut pc = FakePc::new(Facts {
        reaper: true,
        ..iemmixer_up()
    });
    assert_eq!(start(&mut pc, &mut g, 5_000), None);
    assert_eq!(g.state.mode, Mode::Dev);
    // REAPER without our engine is the band's system: event.
    save_state(dir.path(), &mut saved, 9_000);
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    let mut pc = FakePc::new(band_up());
    assert_eq!(start(&mut pc, &mut g, 5_000), Some(Outcome::Done));
    assert_eq!(g.state.mode, Mode::Event);
}

#[test]
fn a_restarted_guard_unwinds_a_half_done_dev_switch() {
    let mut g = Guard::for_test(Mode::Event);
    g.state.switching = Some(Switching {
        from: Mode::Event,
        to: Mode::Dev,
        done: vec![Step::Precheck, Step::AppStop, Step::ReaperSaveQuit],
        started: T0,
    });
    g.state.written_at = 2_000;
    g.state.pins.current = Some(SHA.into());
    let saved_kids = crate::state::Children {
        engine: Some(crate::state::Child {
            pid: 4242,
            start_time: 1,
            image: "iem-engine.exe".into(),
        }),
        ..crate::state::Children::default()
    };
    g.state.pids = saved_kids.clone();
    // REAPER and the app were quit, the engine never came up.
    let mut pc = FakePc::new(Facts::default());
    assert_eq!(start(&mut pc, &mut g, 1_000), Some(Outcome::Done));
    assert_eq!(g.state.mode, Mode::Event);
    assert_eq!(g.state.switching, None);
    assert!(pc.index(Call::ReaperStart) < pc.index(Call::AppStart));
    assert!(!pc.called(Call::EngineStart));
    assert!(pc.index(Call::Adopt) < pc.index(Call::ReaperStart));
    assert_eq!(pc.kids, saved_kids);
    assert_eq!(pc.bundle.as_deref(), Some(SHA));
    // From event these are the start's checks: the fence stays (#42).
    let v = g.shared.view();
    assert_eq!(
        (v.epoch, v.fence, v.start_checks, v.began),
        (1, 0, true, Some((Mode::Event, Mode::Event)))
    );
    // An unfinished event plan resumes the same way.
    let mut g = Guard::for_test(Mode::Dev);
    g.state.switching = Some(Switching {
        from: Mode::Dev,
        to: Mode::Event,
        done: vec![Step::EngineStop],
        started: T0,
    });
    g.state.written_at = 2_000;
    let mut pc = FakePc::new(Facts {
        server: true,
        ..Facts::default()
    });
    assert_eq!(start(&mut pc, &mut g, 1_000), Some(Outcome::Done));
    assert!(pc.index(Call::ServerStop) < pc.index(Call::ReaperStart));
    // From dev it is a way back to REAPER, no start's checks: the fence
    // moved (#42).
    let v = g.shared.view();
    assert_eq!(
        (v.epoch, v.fence, v.start_checks, v.began),
        (1, 1, false, Some((Mode::Dev, Mode::Event)))
    );
}

/// Waits (on a test thread) until the guard runs a switch.
fn await_running(shared: &Shared) {
    let t = Instant::now();
    while shared.view().running.is_none() {
        assert!(t.elapsed() < Duration::from_secs(5), "no switch began");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A PC in dev rebooted comes back in event, and the guard runs the event
/// plan's checks while the pipe already routes (#42). They are the reset
/// rule's checks, not a switch back to REAPER the owner or the crash loop
/// chose: a dev queued before them runs after them, one `iempc dev`, never a
/// repeat (the epoch moved, the fence did not).
#[test]
fn a_dev_queued_before_the_start_checks_runs_after_them() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Dev));
    g.state.pins.current = Some(SHA.into());
    let Route::Queue(seen) = g.shared.route(&dev()) else {
        panic!("an idle guard queues a dev entry");
    };
    assert_eq!(seen, INIT);
    assert_eq!(start(&mut pc, &mut g, 5_000), Some(Outcome::Done));
    assert_eq!(g.state.mode, Mode::Event);
    let v = g.shared.view();
    assert_eq!(
        (v.epoch, v.fence, v.start_checks, v.began),
        (1, 0, true, Some((Mode::Event, Mode::Event)))
    );
    let r = handle(&mut pc, &mut g, dev(), seen);
    assert!(r.ok, "{r:?}");
    assert!(r.detail.starts_with("dev: done"), "{}", r.detail);
    assert_eq!(g.state.mode, Mode::Dev);
    assert!(pc.index(Call::Fingerprint) < pc.index(Call::ReaperSaveQuit));
    assert!(pc.called(Call::EngineArm));
}

/// A dev or live entry routed while the start's checks run is queued behind
/// them instead of refused (#42), and runs after them. Anything else is
/// refused as during any switch.
#[test]
fn a_dev_routed_during_the_start_checks_is_queued_and_runs_after_them() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    pc.delay(Call::Fingerprint, Duration::from_millis(500));
    let shared = Arc::clone(&g.shared);
    let routed = std::thread::spawn(move || {
        await_running(&shared);
        let live = Request::Live {
            build: Some(SHA.into()),
            trial: false,
            dry_run: false,
        };
        [dev(), live, Request::JobBegin { run: 7 }].map(|req| shared.route(&req))
    });
    assert_eq!(start(&mut pc, &mut g, 5_000), Some(Outcome::Done));
    let [dev_route, live_route, job_route] = routed.join().unwrap();
    let during = Generation { epoch: 1, fence: 0 };
    assert_eq!(dev_route, Route::Queue(during));
    assert_eq!(live_route, Route::Queue(during));
    match job_route {
        Route::Now(r) => assert_eq!((r.ok, r.detail.as_str()), (false, "switching")),
        other => panic!("{other:?}"),
    }
    let r = handle(&mut pc, &mut g, dev(), during);
    assert!(r.ok, "{r:?}");
    assert!(r.detail.starts_with("dev: done"), "{}", r.detail);
    assert_eq!(g.state.mode, Mode::Dev);
    assert!(pc.index(Call::Fingerprint) < pc.index(Call::ReaperSaveQuit));
}

/// An "ide event" routed while the start's checks run moves the fence (#42):
/// the checks answer it, and a dev queued before it, before the checks or
/// during them, never runs after it. Routed while no switch runs, it fences a
/// dev queued ahead of it as well; a request that is no entry keeps the
/// generation rule and runs.
#[test]
fn ide_event_during_the_start_checks_fences_a_dev_queued_before_it() {
    let asked = "busy: a switch to event was asked meanwhile";
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    let Route::Queue(before) = g.shared.route(&dev()) else {
        panic!("an idle guard queues a dev entry");
    };
    pc.delay(Call::Fingerprint, Duration::from_millis(500));
    let shared = Arc::clone(&g.shared);
    let routed = std::thread::spawn(move || {
        await_running(&shared);
        let during = shared.route(&dev());
        let event = shared.route(&Request::Event { dry_run: false });
        let answer = shared.await_end("already switching to event", Duration::from_secs(10));
        (during, event, answer)
    });
    assert_eq!(start(&mut pc, &mut g, 5_000), Some(Outcome::Done));
    let (during, event, answer) = routed.join().unwrap();
    assert_eq!(during, Route::Queue(Generation { epoch: 1, fence: 0 }));
    assert_eq!(event, Route::AwaitEnd("already switching to event"));
    assert!(answer.ok, "{answer:?}");
    assert_eq!(g.shared.generation(), Generation { epoch: 1, fence: 1 });
    let made = pc.calls().len();
    // Queued before the checks: they move no fence, the "ide event" routed
    // during them did, so the reply names the event, not the checks.
    let r = handle(&mut pc, &mut g, dev(), before);
    assert_eq!(
        (r.ok, r.detail.as_str(), r.mode),
        (false, asked, Mode::Event)
    );
    // Queued during them, before the "ide event".
    let Route::Queue(seen) = during else {
        unreachable!("asserted above")
    };
    let r = handle(&mut pc, &mut g, dev(), seen);
    assert_eq!((r.ok, r.detail.as_str()), (false, asked));
    assert_eq!(pc.calls().len(), made, "no dev entry ran");
    assert_eq!(g.state.mode, Mode::Event);
    // No switch runs: a dev and a job are queued, then "ide event".
    let mut queue = [
        dev(),
        Request::AlarmAck { id: 99 },
        Request::Event { dry_run: false },
    ]
    .map(|req| match g.shared.route(&req) {
        Route::Queue(seen) => (req, seen),
        other => panic!("{req:?}: {other:?}"),
    })
    .into_iter();
    let (req, seen) = queue.next().unwrap();
    let r = handle(&mut pc, &mut g, req, seen);
    assert_eq!((r.ok, r.detail.as_str()), (false, asked));
    let (req, seen) = queue.next().unwrap();
    assert_eq!(seen, Generation { epoch: 1, fence: 1 });
    let r = handle(&mut pc, &mut g, req, seen);
    assert_eq!((r.ok, r.detail.as_str()), (false, "no alarm 99"));
    let (req, seen) = queue.next().unwrap();
    assert_eq!(seen, Generation { epoch: 1, fence: 2 });
    let r = handle(&mut pc, &mut g, req, seen);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Event);
    assert!(!pc.called(Call::ReaperSaveQuit), "no dev entry ran");
}

#[test]
fn direct_event_runs_without_a_guard() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    let r = direct_event(&mut pc, &mut g, Some(()), false);
    assert!(r.ok, "{r:?}");
    assert!(r.detail.starts_with("direct: event: done"), "{}", r.detail);
    assert_eq!(r.mode, Mode::Event);
    assert!(pc.index(Call::EngineStop) < pc.index(Call::ReaperStart));
    assert!(pc.index(Call::ReaperStart) < pc.index(Call::AppStart));
    assert!(pc.index(Call::Adopt) < pc.index(Call::EngineStop));
    // With the mutex taken it refuses and touches nothing.
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    let r = direct_event::<()>(&mut pc, &mut g, None, false);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "a guard runs; use the pipe")
    );
    assert_eq!(r.mode, Mode::Dev);
    assert!(pc.calls().is_empty());
    // Its dry run shows the plan only.
    let r = direct_event(&mut pc, &mut g, Some(()), true);
    assert!(r.ok);
    assert_eq!(
        r.detail,
        "direct: dry run: EngineStop, ServerStop, TrayStop, TuningExit, PrefCheck, \
         ReaperStart, ReaperHandover, AppStart, AppHandover, Fingerprint"
    );
    assert!(pc.mutating_calls().is_empty());
    assert_eq!(g.state.mode, Mode::Dev);
    // Its alarms go out before it ends.
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    pc.fail(Call::Fingerprint, "the tuning task did not answer");
    let r = direct_event(&mut pc, &mut g, Some(()), false);
    assert!(r.ok);
    assert_eq!(pc.calls().last(), Some(&Call::Notify));
}

// ---- files, install, activate ----

#[test]
fn the_state_and_alarms_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    assert_eq!(g.root(), Some(dir.path()));
    g.raise(Some(Step::PrefCheck), "kept", true);
    g.state.pins.current = Some(SHA.into());
    g.save();
    let back = Guard::open(dir.path(), SiteConf::default(), fixed(T0 + 5));
    assert_eq!(back.state.pins.current.as_deref(), Some(SHA));
    assert_eq!(back.alarms, g.alarms);
    assert_eq!(back.state.written_at, T0);
    assert_eq!(back.shared.view().alarms.len(), 1);
    // Unreadable files start from the defaults with an alarm each.
    let files = dir.path().join("guard");
    std::fs::write(files.join(STATE_FILE), b"{").unwrap();
    std::fs::write(files.join(ALARMS_FILE), b"[").unwrap();
    let g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    assert_eq!(g.state.mode, Mode::Event);
    let t = texts(&g);
    assert_eq!(t.len(), 2);
    assert!(t[0].starts_with("guard state unreadable ("), "{t:?}");
    assert!(t[1].starts_with("guard alarms unreadable ("), "{t:?}");
    assert_eq!(
        (STATE_FILE, ALARMS_FILE),
        ("guard-state.json", "alarms.json")
    );
}

#[test]
fn install_records_the_bundle_and_refuses_other_sums() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    let mut pc = FakePc::new(Facts::default());
    let zip = install::tests::good_zip(dir.path(), SHA);
    let r = handle(
        &mut pc,
        &mut g,
        Request::Install {
            zip: zip.to_string_lossy().into_owned(),
        },
        INIT,
    );
    assert_eq!(
        (r.ok, r.detail.clone()),
        (true, format!("bundle {SHA} installed"))
    );
    assert_eq!(g.state.bundles[SHA], record(SHA, "dev", Hil::Pending));
    let r = handle(
        &mut pc,
        &mut g,
        Request::Install {
            zip: zip.to_string_lossy().into_owned(),
        },
        INIT,
    );
    assert_eq!(r.detail, format!("bundle {SHA} already installed"));
    // Other sums for the same SHA: refused with an alarm.
    let mut entries = install::tests::files(SHA);
    if let Some(e) = entries.iter_mut().find(|(n, _)| n == "hil-v1.ps1") {
        e.1 = b"changed".to_vec();
    }
    let sums = install::tests::sums(&entries);
    entries.push((crate::bundle::SUMS.to_owned(), sums.into_bytes()));
    let other = dir.path().join("other.zip");
    install::tests::zip_of(&other, &entries);
    let r = handle(
        &mut pc,
        &mut g,
        Request::Install {
            zip: other.to_string_lossy().into_owned(),
        },
        INIT,
    );
    let why = format!("bundle {SHA} is installed with other sums; it is never overwritten");
    assert_eq!((r.ok, r.detail.clone()), (false, why.clone()));
    assert_eq!(texts(&g), [why]);
    // A broken zip: refused, no alarm.
    let r = handle(
        &mut pc,
        &mut g,
        Request::Install {
            zip: dir.path().join("none.zip").to_string_lossy().into_owned(),
        },
        INIT,
    );
    assert!(!r.ok);
    assert!(r.detail.starts_with("install refused: "), "{}", r.detail);
    assert_eq!(g.alarms.all().len(), 1);
    // Without files there is nothing to install into.
    let mut bare = Guard::for_test(Mode::Dev);
    assert_eq!(
        install_bundle(&mut bare, &zip),
        (false, "the guard has no bundle directory".to_owned())
    );
}

#[test]
fn activation_sets_the_active_bundle_copies_excludes_and_hands_over() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    g.state.mode = Mode::Dev;
    let mut pc = FakePc::new(Facts::default());
    let activate = Request::Activate { sha: SHA.into() };
    let r = handle(&mut pc, &mut g, activate.clone(), INIT);
    assert_eq!(r.detail, format!("bundle {SHA} is not installed"));
    let zip = install::tests::good_zip(dir.path(), SHA);
    assert!(install_bundle(&mut g, &zip).0);
    let r = handle(&mut pc, &mut g, activate.clone(), INIT);
    assert_eq!(
        (r.ok, r.detail.clone()),
        (
            true,
            format!("activated {SHA}; the guard hands over to its new exe")
        )
    );
    let bin = install::bin_dir(dir.path());
    assert_eq!(g.handover, Some(bin.join(install::GUARD_EXE)));
    assert!(bin.join(install::IEMMODE_EXE).is_file());
    // S8 (#11): the active bundle, never the pin.
    assert_eq!(g.state.active_bundle(), Some(SHA));
    assert_eq!(pc.bundle.as_deref(), Some(SHA));
    assert!(pc.called(Call::Exclude));
    // The same bundle again: the guard's exe did not change.
    g.handover = None;
    let r = handle(&mut pc, &mut g, activate.clone(), INIT);
    assert_eq!((r.ok, r.detail.clone()), (true, format!("activated {SHA}")));
    assert_eq!(g.handover, None);
    // Failed exclusions alarm, the activation stands.
    pc.fail(Call::Exclude, "the exclude task ended with 1");
    let r = handle(&mut pc, &mut g, activate, INIT);
    assert!(r.ok);
    assert_eq!(
        texts(&g),
        [format!(
            "Defender exclusions for {SHA}: the exclude task ended with 1"
        )]
    );
    // Without files the activation fails.
    let (mut pc, mut bare) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Dev));
    bare.state
        .bundles
        .insert(SHA.into(), record(SHA, "dev", Hil::Pending));
    let r = handle(
        &mut pc,
        &mut bare,
        Request::Activate { sha: SHA.into() },
        INIT,
    );
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (
            false,
            "activation failed: the guard has no bundle directory"
        )
    );
    assert!(!pc.called(Call::Exclude));
}

#[test]
fn the_first_offline_install_activates_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    let zip = install::tests::good_zip(dir.path(), SHA);
    assert_eq!(
        install_offline(&mut g, &zip),
        (true, format!("bundle {SHA} installed; activated into bin"))
    );
    assert_eq!(g.state.active_bundle(), Some(SHA));
    assert!(
        install::bin_dir(dir.path())
            .join(install::GUARD_EXE)
            .is_file()
    );
    // A later one is only installed.
    let zip = install::tests::good_zip(dir.path(), OTHER);
    assert_eq!(
        install_offline(&mut g, &zip),
        (true, format!("bundle {OTHER} installed"))
    );
    assert_eq!(g.state.active_bundle(), Some(SHA));
    // A refused zip is reported as such.
    let (ok, why) = install_offline(&mut g, &dir.path().join("none.zip"));
    assert!(!ok);
    assert!(why.starts_with("install refused: "), "{why}");
    // The saved state carries both records.
    let back = Guard::open(dir.path(), SiteConf::default(), fixed(T0));
    assert_eq!(back.state.bundles.len(), 2);
}

#[test]
fn site_settings_and_clocks() {
    let s = crate::site::tests::settings();
    assert_eq!(
        SiteConf::from_site(&s.guard),
        SiteConf {
            on_pref_fail: PrefFail::StartReaperWithAlarm,
            hil_tx: vec![94, 95],
        }
    );
    assert_eq!(
        SiteConf::default(),
        SiteConf {
            on_pref_fail: PrefFail::StartReaperWithAlarm,
            hil_tx: Vec::new(),
        }
    );
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let sys = Clock::System.now();
    assert!((now..=now + 5).contains(&sys), "{sys} {now}");
    assert_eq!(fixed(42).now(), 42);
}

#[test]
fn a_rehearsal_names_a_single_process_that_stays_up() {
    // The teardown's after-check ORs engine/server/tray: a lone stubborn
    // process (here only the server, whose stop is refused) must still be
    // reported. If the first `||` were `&&` it would take TWO up to report any.
    let (mut pc, mut g) = (
        FakePc::new(Facts {
            server: true,
            ..Facts::default()
        }),
        Guard::for_test(Mode::Dev),
    );
    g.state.pins.current = Some(SHA.into());
    pc.fail(Call::ServerStop, "the server did not stop");
    let r = ask(&mut pc, &mut g, Request::RehearseTeardown);
    assert!(!r.ok, "{r:?}");
    assert!(
        r.detail.contains("iemmixer processes still run"),
        "a lone server left up must still be named: {}",
        r.detail
    );
}
