//! The guard pipe's requests against `FakePc` (`daemon/requests.rs`): the
//! routing while a switch runs, the requests queued before a switch (#42),
//! the dry runs, the dev and live entries' prechecks and bundles, and the
//! HIL job's and test signal's gates.

use std::time::{Duration, Instant};

use super::tests::{
    INIT, OTHER, SHA, ask, band_up, dev, iemmixer_up, record, status_reply, steps, texts,
};
use super::*;
use crate::bundle::{Hil, Pins};
use crate::lifecycle::{Lifecycle, Prod};
use crate::pc::Kid;
use crate::pc::fake::{Call, FakePc};
use crate::plan::Facts;
use crate::proto::Request;

/// Prod on `pin` (S8): a live entry that is no trial is prod's; before the
/// cutover live is a trial.
fn prod_on(pin: &str) -> Lifecycle {
    Lifecycle::Prod(Prod {
        since: 1_790_000_000,
        pin: pin.to_owned(),
        previous: None,
        maintenance: None,
    })
}

#[test]
fn jobs_are_refused_while_switching() {
    let shared = Shared::new(Cancel::default());
    shared.update(|v| {
        v.running = Some(Mode::Dev);
        v.status = "mode event".into();
    });
    let refused = [
        Request::JobBegin { run: 1 },
        Request::JobEnd { run: 1 },
        Request::Install {
            zip: "b.zip".into(),
        },
        Request::Activate { sha: SHA.into() },
        Request::TestSignal {
            input: "mic1".into(),
            dbfs: -30.0,
            ttl_s: 5.0,
            listen: false,
        },
        Request::Report {
            sha: SHA.into(),
            hil: "green".into(),
            detail: "x".into(),
        },
        Request::InstallSite {
            path: "site.toml".into(),
        },
        Request::ForceReopen,
        Request::InjectFault,
        Request::InjectSeh,
        Request::InjectPark,
        Request::RunnerStop,
        Request::ProbeTask,
        Request::RehearseTeardown,
        Request::AlarmTest,
        Request::AlarmAck { id: 1 },
        Request::Quit,
        Request::Event { dry_run: true },
    ];
    for req in &refused {
        match shared.route(req) {
            Route::Now(r) => assert_eq!((r.ok, r.detail.as_str()), (false, "switching"), "{req:?}"),
            other => panic!("{req:?}: {other:?}"),
        }
    }
    let live = Request::Live {
        build: SHA.into(),
        trial: false,
        dry_run: false,
    };
    for req in [dev(), live] {
        match shared.route(&req) {
            Route::Now(r) => assert_eq!((r.ok, r.detail.as_str()), (false, "busy")),
            other => panic!("{other:?}"),
        }
    }
    match shared.route(&Request::Status) {
        Route::Now(r) => assert_eq!((r.ok, r.detail.as_str()), (true, "mode event")),
        other => panic!("{other:?}"),
    }
    assert_eq!(shared.route(&Request::Subscribe), Route::Subscribe);
    // A job queued before a switch began is answered as during it; a dev or
    // live entry once the fence moved too (every switch but the start's
    // checks moves it, #42), naming the switch.
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    let seen = g.shared.generation();
    g.shared.update(|v| {
        v.epoch += 1;
        v.fence += 1;
        v.began = Some((Mode::Dev, Mode::Event));
    });
    let r = handle(&mut pc, &mut g, Request::JobBegin { run: 4242 }, seen);
    assert_eq!((r.ok, r.detail.as_str()), (false, "switching"));
    assert_eq!(g.state.job, None);
    let r = handle(&mut pc, &mut g, dev(), seen);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "busy: a switch ran meanwhile (dev → event)")
    );
    assert!(pc.calls().is_empty());
    let r = handle(&mut pc, &mut g, Request::Event { dry_run: false }, seen);
    assert!(!r.ok);
    assert_eq!(r.detail, "a switch ran meanwhile; event: no switch yet");
    assert!(pc.calls().is_empty());
    let r = handle(&mut pc, &mut g, Request::Status, seen);
    assert!(r.ok);
    // Only the epoch moved (the start's checks ran): a job is still answered
    // as during them, a dev entry is handled.
    let seen = g.shared.generation();
    assert_eq!(seen, Generation { epoch: 1, fence: 1 });
    g.shared.update(|v| v.epoch += 1);
    let r = handle(&mut pc, &mut g, Request::JobBegin { run: 4242 }, seen);
    assert_eq!((r.ok, r.detail.as_str()), (false, "switching"));
    let dry_dev = Request::Dev {
        build: None,
        dry_run: true,
    };
    let r = handle(&mut pc, &mut g, dry_dev, seen);
    assert!(r.ok, "{r:?}");
    assert!(r.detail.starts_with("dry run: "), "{}", r.detail);
    // Only the fence moved (an "ide event" was routed): the job is handled,
    // the dev entry is not.
    let seen = g.shared.generation();
    g.shared.update(|v| v.fence += 1);
    let r = handle(&mut pc, &mut g, dev(), seen);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "busy: a switch to event was asked meanwhile")
    );
    assert_eq!(g.state.mode, Mode::Dev);
    let r = handle(&mut pc, &mut g, Request::JobBegin { run: 4242 }, seen);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.job, Some(4242));
}

#[test]
fn ide_event_preempts_or_waits_but_never_both() {
    let cancel = Cancel::default();
    let shared = Shared::new(cancel.clone());
    // Idle: pre-empt whatever is queued ahead, then queue. Every "ide event"
    // moves the fence, so a dev or live entry queued before it never runs
    // after it (#42).
    assert_eq!(
        shared.route(&Request::Event { dry_run: false }),
        Route::Queue(Generation { epoch: 0, fence: 1 })
    );
    assert!(cancel.preempted());
    cancel.clear();
    // During an event plan: wait for it, the token untouched.
    shared.update(|v| {
        v.running = Some(Mode::Event);
        v.epoch = 3;
    });
    assert_eq!(
        shared.route(&Request::Event { dry_run: false }),
        Route::AwaitEnd("already switching to event")
    );
    assert!(!cancel.preempted());
    assert_eq!(shared.view().fence, 2);
    // During any other switch: pre-empt it and wait.
    shared.update(|v| v.running = Some(Mode::Live));
    assert_eq!(
        shared.route(&Request::Event { dry_run: false }),
        Route::AwaitEnd("pre-empted the switch in progress")
    );
    assert!(cancel.preempted());
    assert_eq!(shared.view().fence, 3);
    // Other requests while idle go to the daemon with the generation; they
    // move no fence (a dry run neither).
    shared.update(|v| v.running = None);
    let now = Generation { epoch: 3, fence: 3 };
    assert_eq!(shared.route(&Request::Quit), Route::Queue(now));
    assert_eq!(
        shared.route(&Request::Event { dry_run: true }),
        Route::Queue(now)
    );
    assert_eq!(shared.route(&dev()), Route::Queue(now));
    assert!(matches!(shared.route(&Request::Status), Route::Now(_)));
    assert_eq!(shared.route(&Request::Subscribe), Route::Subscribe);
    assert_eq!(shared.view().generation(), now);
}

/// A dev queued before a switch that is not the start's checks never runs
/// after it (#42): the crash loop's way back to REAPER, an "ide event" (from
/// dev, or its checks in event), an entry that unwound. The reply names the
/// switch that ran meanwhile; a live entry is answered the same.
#[test]
fn an_entry_queued_before_a_switch_back_to_reaper_never_runs() {
    let ran = |from: &str, to: &str| format!("busy: a switch ran meanwhile ({from} → {to})");
    let live = Request::Live {
        build: SHA.into(),
        trial: false,
        dry_run: false,
    };
    // The crash loop goes back to REAPER: dev → event.
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Dev));
    let seen = g.shared.generation();
    pc.exited = vec![(Kid::Engine, Some(70)); 3];
    tick(&mut pc, &mut g, Instant::now());
    assert_eq!(g.state.mode, Mode::Event);
    let made = pc.calls().len();
    for req in [dev(), live.clone()] {
        let r = handle(&mut pc, &mut g, req, seen);
        assert_eq!(
            (r.ok, r.detail, r.mode),
            (false, ran("dev", "event"), Mode::Event)
        );
    }
    assert_eq!(pc.calls().len(), made, "nothing ran");
    // "Ide event" from dev: dev → event.
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    let seen = g.shared.generation();
    assert!(ask(&mut pc, &mut g, Request::Event { dry_run: false }).ok);
    let made = pc.calls().len();
    let r = handle(&mut pc, &mut g, dev(), seen);
    assert_eq!((r.ok, r.detail), (false, ran("dev", "event")));
    assert_eq!(pc.calls().len(), made, "nothing ran");
    assert_eq!(g.state.mode, Mode::Event);
    // "Ide event" in event runs its checks (event → event), which are not
    // the start's.
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    let seen = g.shared.generation();
    assert!(ask(&mut pc, &mut g, Request::Event { dry_run: false }).ok);
    let made = pc.calls().len();
    for req in [dev(), live] {
        let r = handle(&mut pc, &mut g, req, seen);
        assert_eq!((r.ok, r.detail), (false, ran("event", "event")));
    }
    assert_eq!(pc.calls().len(), made, "nothing ran");
    // An entry that failed and unwound (event → dev, then event → event).
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    let seen = g.shared.generation();
    pc.fail(Call::Data, "iem-migrate band ended with Some(1)");
    assert!(!ask(&mut pc, &mut g, dev()).ok);
    assert_eq!(g.state.mode, Mode::Event);
    let made = pc.calls().len();
    let r = handle(&mut pc, &mut g, dev(), seen);
    assert_eq!((r.ok, r.detail), (false, ran("event", "event")));
    assert_eq!(pc.calls().len(), made, "nothing ran");
    assert_eq!(pc.count(Call::ReaperSaveQuit), 1, "the failed entry's only");
}

#[test]
fn await_end_answers_when_the_switch_ended() {
    let shared = Arc::new(Shared::new(Cancel::default()));
    shared.update(|v| v.running = Some(Mode::Event));
    let other = Arc::clone(&shared);
    let t = Instant::now();
    let ender = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        other.update(|v| {
            v.running = None;
            v.mode = Mode::Event;
            v.last = Some(Outcome::Done);
        });
    });
    let r = shared.await_end("already switching to event", Duration::from_secs(5));
    ender.join().unwrap();
    assert!(t.elapsed() >= Duration::from_millis(200));
    assert!(t.elapsed() < Duration::from_secs(3));
    assert!(r.ok);
    assert_eq!(r.detail, "already switching to event; event: done");
    // A switch that does not end: not done after the limit.
    shared.update(|v| v.running = Some(Mode::Event));
    let t = Instant::now();
    let r = shared.await_end("waited", Duration::from_millis(100));
    assert!(t.elapsed() >= Duration::from_millis(100));
    assert!(!r.ok);
    // Ended, but not in event, or not done.
    for (mode, last) in [
        (Mode::Dev, Some(Outcome::Done)),
        (Mode::Event, Some(Outcome::NeedsOwner)),
        (Mode::Event, None),
    ] {
        shared.update(|v| {
            v.running = None;
            v.mode = mode;
            v.last = last;
        });
        assert!(
            !shared.await_end("x", Duration::ZERO).ok,
            "{mode:?} {last:?}"
        );
    }
}

#[test]
fn a_tray_quit_waits_for_the_next_subscriber() {
    let shared = Shared::new(Cancel::default());
    // Without a subscriber the quit waits: the tray may be in its 2 s retry.
    assert_eq!(shared.tray_quit(), Ok(()));
    assert!(shared.take_tray_quit());
    assert!(!shared.take_tray_quit(), "a quit is delivered once");
    shared.add_subscriber();
    assert_eq!(shared.tray_quit(), Ok(()));
    assert_eq!(shared.view().tray_quits, 2);
    // It wakes the subscriber at once.
    let seen = (shared.view().version, 1);
    let t = Instant::now();
    let v = shared.wait_change(seen, Duration::from_secs(5));
    assert!(t.elapsed() < Duration::from_secs(1));
    assert_eq!(v.tray_quits, 2);
    // A tray start forgets a quit no tray took.
    shared.clear_tray_quit();
    assert!(!shared.take_tray_quit());
    shared.drop_subscriber();
    shared.drop_subscriber();
    assert_eq!(shared.view().subscribers, 0);
    // No change: the wait ends at its limit.
    let v = shared.view();
    let t = Instant::now();
    shared.wait_change((v.version, v.tray_quits), Duration::from_millis(100));
    assert!(t.elapsed() >= Duration::from_millis(100));
}

#[test]
fn a_tray_start_forgets_a_quit_no_tray_took() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    // A stop whose tray never came back to take its quit.
    g.shared.tray_quit().unwrap();
    run_switch(&mut pc, &mut g, Mode::Event, Mode::Dev);
    assert!(pc.called(Call::TrayStart));
    assert_eq!(g.state.mode, Mode::Dev);
    assert!(
        !g.shared.take_tray_quit(),
        "the new tray must not quit at once"
    );
}

#[test]
fn dry_run_changes_nothing() {
    // S8 (#11): before the cutover live is a trial.
    let live = Request::Live {
        build: SHA.into(),
        trial: true,
        dry_run: true,
    };
    let dry_dev = Request::Dev {
        build: None,
        dry_run: true,
    };
    for req in [Request::Event { dry_run: true }, dry_dev, live] {
        let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
        g.state
            .bundles
            .insert(SHA.into(), record(SHA, "main", Hil::Green));
        let r = handle(&mut pc, &mut g, req.clone(), INIT);
        assert!(r.ok, "{req:?}: {r:?}");
        assert!(r.detail.starts_with("dry run: "), "{}", r.detail);
        assert_eq!(pc.mutating_calls(), Vec::<Call>::new(), "{req:?}");
        assert_eq!(g.state.mode, Mode::Event);
        assert_eq!(g.state.pins, Pins::default());
        assert_eq!(g.shared.view().epoch, 0);
    }
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    let r = handle(
        &mut pc,
        &mut g,
        Request::Dev {
            build: None,
            dry_run: true,
        },
        INIT,
    );
    assert_eq!(
        r.detail,
        "dry run: Precheck, AppStop, ReaperSaveQuit, TuningEnter, Data, PrefCheck, EngineStart, \
         EngineArm, ServerStart, TrayStart, IdentityCheck, RunnerStart; bundle none; precheck ok"
    );
    pc.fail(Call::Precheck, "an engine the guard did not start runs");
    let r = handle(
        &mut pc,
        &mut g,
        Request::Dev {
            build: None,
            dry_run: true,
        },
        INIT,
    );
    assert!(!r.ok);
    assert!(
        r.detail
            .ends_with("RunnerStart; bundle none; precheck an engine the guard did not start runs"),
        "{}",
        r.detail
    );
    let r = handle(&mut pc, &mut g, Request::Event { dry_run: true }, INIT);
    assert_eq!(
        r.detail,
        "dry run: TuningExit, PrefCheck, ReaperHandover, AppHandover, Fingerprint"
    );
}

/// The precheck's text for a missing PWA notification subscription.
const NO_SUBSCRIPTION: &str =
    "no PWA notification subscription: no engineer device allowed notifications";
/// Its dev note (`pc::precheck`).
const NO_SUBSCRIPTION_NOTE: &str = "no PWA notification subscription: no engineer device \
     allowed notifications (not needed for dev: the alarms stay in the guard's alarm file)";

/// The alarms go to the engineer's PWA notification subscriptions (#9
/// 2026-09-28); the predecessor's arrive with the band import, a later step
/// of the entry, and a new one only through iem-server (dev and live): a
/// dev entry without one goes on and names it, in its report and in the
/// status until an alarm reaches a phone.
#[test]
fn a_dev_entry_without_a_pwa_subscription_goes_on_and_names_it() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    pc.subscriptions = Some(0);
    let r = ask(&mut pc, &mut g, dev());
    assert!(r.ok, "{r:?}");
    assert!(
        r.detail.starts_with(&format!(
            "dev: done; {NO_SUBSCRIPTION_NOTE}; tuning enter: "
        )),
        "{}",
        r.detail
    );
    assert_eq!(g.state.mode, Mode::Dev);
    assert!(g.alarms.all().is_empty(), "{:?}", texts(&g));
    assert_eq!(
        status_reply(&g),
        format!("mode dev; bundle {SHA}; {NO_SUBSCRIPTION_NOTE}")
    );
    assert_eq!(
        ask(&mut pc, &mut g, Request::Status).detail,
        format!("mode dev; bundle {SHA}; {NO_SUBSCRIPTION_NOTE}")
    );
    // Its dry run passes and names it too.
    let dry = Request::Dev {
        build: None,
        dry_run: true,
    };
    let r = ask(&mut pc, &mut g, dry);
    assert!(r.ok, "{r:?}");
    assert!(
        r.detail.ends_with(&format!(
            "; bundle {SHA}; precheck ok; {NO_SUBSCRIPTION_NOTE}"
        )),
        "{}",
        r.detail
    );
    // An alarm that does not reach a phone keeps it named.
    pc.fail(Call::Notify, "no device took the notice");
    let r = ask(&mut pc, &mut g, Request::AlarmTest);
    assert!(!r.ok, "{r:?}");
    assert!(
        status_reply(&g).contains(NO_SUBSCRIPTION_NOTE),
        "{}",
        status_reply(&g)
    );
    // The engineer allows notifications in the mixer app (iem-server now
    // serves it) and the alarm test reaches the phone: the status no
    // longer names it.
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    pc.subscriptions = Some(0);
    assert!(ask(&mut pc, &mut g, dev()).ok);
    pc.subscriptions = Some(1);
    let r = ask(&mut pc, &mut g, Request::AlarmTest);
    assert!(r.ok, "{r:?}");
    assert!(
        !status_reply(&g).contains(NO_SUBSCRIPTION),
        "{}",
        status_reply(&g)
    );
    // With a subscription the entry names nothing.
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    let r = ask(&mut pc, &mut g, dev());
    assert!(r.ok, "{r:?}");
    assert!(!r.detail.contains("PWA notification"), "{}", r.detail);
    assert_eq!(status_reply(&g), format!("mode dev; bundle {SHA}"));
}

/// What `tls::check` names on the PC (#9 2026-09-28): LAN 443 serves the
/// certificate `iem-migrate band` took over, and the predecessor serves the
/// same one, expired months ago.
const LAN_NOTE: &str =
    "the LAN certificate expired on 2026-03-01; the predecessor serves the same one";

/// The identity check proves identity, not validity: a certificate outside
/// its validity is named once in the entry's report and in the status
/// until the next check, never an alarm.
#[test]
fn an_expired_lan_certificate_is_named_and_never_an_alarm() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    pc.lan_note = Some(LAN_NOTE.into());
    let r = ask(&mut pc, &mut g, dev());
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Dev);
    assert_eq!(r.detail.matches(LAN_NOTE).count(), 1, "{}", r.detail);
    assert!(r.detail.contains(&format!("; {LAN_NOTE}")), "{}", r.detail);
    // Reported where the check ran: after the server started.
    assert!(
        r.detail.find("server started") < r.detail.find(LAN_NOTE),
        "{}",
        r.detail
    );
    assert!(g.alarms.all().is_empty(), "{:?}", texts(&g));
    assert_eq!(
        status_reply(&g),
        format!("mode dev; bundle {SHA}; {LAN_NOTE}")
    );
    assert_eq!(
        ask(&mut pc, &mut g, Request::Status).detail,
        format!("mode dev; bundle {SHA}; {LAN_NOTE}")
    );
    // Back in event the predecessor serves the same certificate: still
    // named.
    assert!(ask(&mut pc, &mut g, Request::Event { dry_run: false }).ok);
    assert!(status_reply(&g).contains(LAN_NOTE), "{}", status_reply(&g));
    // The next check that names nothing drops it.
    pc.lan_note = None;
    let r = ask(&mut pc, &mut g, dev());
    assert!(r.ok, "{r:?}");
    assert!(!r.detail.contains("LAN certificate"), "{}", r.detail);
    assert_eq!(status_reply(&g), format!("mode dev; bundle {SHA}"));
    // A failed check does not repeat an old note.
    pc.lan_note = Some(LAN_NOTE.into());
    assert!(ask(&mut pc, &mut g, Request::Event { dry_run: false }).ok);
    assert!(ask(&mut pc, &mut g, dev()).ok);
    assert!(status_reply(&g).contains(LAN_NOTE), "{}", status_reply(&g));
    assert!(ask(&mut pc, &mut g, Request::Event { dry_run: false }).ok);
    pc.fail(Call::Identity, "LAN 443: serves another certificate");
    let r = ask(&mut pc, &mut g, dev());
    assert!(!r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Event);
    assert!(!r.detail.contains(LAN_NOTE), "{}", r.detail);
    assert!(
        !status_reply(&g).contains("LAN certificate"),
        "{}",
        status_reply(&g)
    );
}

#[test]
fn a_live_entry_without_a_pwa_subscription_is_refused() {
    let live = |trial, dry_run| Request::Live {
        build: SHA.into(),
        trial,
        dry_run,
    };
    for trial in [false, true] {
        let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
        g.state
            .bundles
            .insert(SHA.into(), record(SHA, "main", Hil::Green));
        if !trial {
            g.state.lifecycle = prod_on(SHA);
        }
        pc.subscriptions = Some(0);
        let r = ask(&mut pc, &mut g, live(trial, true));
        assert!(!r.ok, "{r:?}");
        assert!(
            r.detail.ends_with(&format!("precheck {NO_SUBSCRIPTION}")),
            "{}",
            r.detail
        );
        let r = ask(&mut pc, &mut g, live(trial, false));
        assert!(!r.ok, "{r:?}");
        assert_eq!(g.state.mode, Mode::Event);
        assert!(!pc.called(Call::EngineStart));
        assert_eq!(texts(&g), [format!("Precheck: {NO_SUBSCRIPTION}")]);
    }
}

/// The precheck's text for a trial before the PC tests (`pc::precheck`).
const NO_PC_TESTS: &str =
    "[guard] pc_tests_passed is false: no trial before the owner-approved PC tests";

/// `live --trial` waits for the owner-approved PC tests (design §10,
/// `[guard] pc_tests_passed`): its entry and its dry run are refused
/// without them and pass with them, and a live entry that is no trial is
/// not affected. The trial reaches the precheck only through the entry
/// (`g.trial = e.trial`, the dry run's `e.trial`; review of PR #39, #38):
/// dropping either passes a trial here.
#[test]
fn a_live_trial_needs_the_pc_tests_and_a_plain_live_entry_does_not() {
    let live = |trial, dry_run| Request::Live {
        build: SHA.into(),
        trial,
        dry_run,
    };
    let fresh = |pc_tests_passed| {
        let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
        g.state
            .bundles
            .insert(SHA.into(), record(SHA, "main", Hil::Green));
        pc.pc_tests_passed = pc_tests_passed;
        (pc, g)
    };
    // Without the PC tests: the trial's dry run and its entry are refused.
    let (mut pc, mut g) = fresh(false);
    let r = ask(&mut pc, &mut g, live(true, true));
    assert!(!r.ok, "{r:?}");
    assert!(
        r.detail.ends_with(&format!("precheck {NO_PC_TESTS}")),
        "{}",
        r.detail
    );
    assert_eq!(pc.mutating_calls(), Vec::<Call>::new());
    let r = ask(&mut pc, &mut g, live(true, false));
    assert!(!r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Event);
    assert!(!pc.called(Call::EngineStart));
    assert_eq!(texts(&g), [format!("Precheck: {NO_PC_TESTS}")]);
    // With them the same trial passes both.
    let (mut pc, mut g) = fresh(true);
    let r = ask(&mut pc, &mut g, live(true, true));
    assert!(r.ok, "{r:?}");
    assert!(r.detail.ends_with("; precheck ok"), "{}", r.detail);
    let r = ask(&mut pc, &mut g, live(true, false));
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Live);
    // A live entry that is no trial (prod's, S8) does not read them.
    let (mut pc, mut g) = fresh(false);
    g.state.lifecycle = prod_on(SHA);
    let r = ask(&mut pc, &mut g, live(false, true));
    assert!(r.ok, "{r:?}");
    assert!(r.detail.ends_with("; precheck ok"), "{}", r.detail);
    let r = ask(&mut pc, &mut g, live(false, false));
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Live);
}

#[test]
fn live_needs_an_installed_green_main_bundle() {
    let live = |trial| Request::Live {
        build: SHA.into(),
        trial,
        dry_run: false,
    };
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    let r = handle(&mut pc, &mut g, live(false), INIT);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, format!("bundle {SHA} is not installed").as_str())
    );
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "dev", Hil::Green));
    let r = handle(&mut pc, &mut g, live(false), INIT);
    assert_eq!(r.detail, format!("{SHA} is from \"dev\"; live needs main"));
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Pending));
    let r = handle(&mut pc, &mut g, live(false), INIT);
    assert_eq!(r.detail, format!("{SHA}: HIL Pending; live needs green"));
    assert!(pc.calls().is_empty());
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Green));
    // S8 (#11): before the cutover live is a trial; a plain live is refused.
    let r = handle(&mut pc, &mut g, live(false), INIT);
    assert_eq!(
        (r.ok, r.detail),
        (
            false,
            format!("before the cutover live is a trial: live --build {SHA} --trial")
        )
    );
    assert!(pc.calls().is_empty());
    // A trial (the band there on purpose) enters, on its build.
    let r = handle(&mut pc, &mut g, live(true), INIT);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Live);
    assert_eq!(g.state.active_bundle(), Some(SHA));
    assert_eq!(pc.bundle.as_deref(), Some(SHA));
    assert!(!pc.called(Call::RunnerStart));
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Green));
    let r = handle(&mut pc, &mut g, live(true), INIT);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Live);
}

/// S8 (#11, design §3.4): a dev entry with a build runs it as the active
/// bundle; the pins stay as they were.
#[test]
fn dev_with_a_build_runs_it() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    let build = |sha: &str| Request::Dev {
        build: Some(sha.into()),
        dry_run: false,
    };
    let r = handle(&mut pc, &mut g, build(SHA), INIT);
    assert_eq!(r.detail, format!("bundle {SHA} is not installed"));
    g.state.pins.current = Some(OTHER.into());
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "dev", Hil::Pending));
    let r = handle(&mut pc, &mut g, build(SHA), INIT);
    assert!(r.ok, "{r:?}");
    assert_eq!(
        g.state.pins,
        Pins {
            current: Some(OTHER.into()),
            previous: None,
        }
    );
    assert_eq!(g.state.active_bundle(), Some(SHA));
    assert_eq!(pc.bundle.as_deref(), Some(SHA));
}

/// #38 (owner, 2026-10-06): a HIL job begins in dev when no other job runs,
/// whatever the stage carries. Nothing reads it: other devices on the Dante
/// network feed the card's inputs, and only the owner's signal decides
/// whether the PC may be used.
#[test]
fn a_hil_job_begins_in_dev_without_reading_the_stage() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    assert_eq!(
        handle(&mut pc, &mut g, Request::JobBegin { run: 7 }, INIT),
        g.reply(true, "HIL job 7 began")
    );
    assert_eq!(g.state.job, Some(7));
    assert_eq!(pc.calls(), Vec::<Call>::new(), "no PC read");
}

/// #38: a dev entry reads no stage, never waits for quiet and never refuses
/// on activity: from the band's system the app's stop follows the precheck,
/// and with nothing running the tuning enter does.
#[test]
fn a_dev_entry_reads_no_stage_and_never_waits_for_quiet() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    let r = handle(&mut pc, &mut g, dev(), INIT);
    assert!(r.ok, "{r:?}");
    assert_eq!(g.state.mode, Mode::Dev);
    assert_eq!(
        steps(&pc).get(..3),
        Some(&[Call::Precheck, Call::AppStop, Call::ReaperSaveQuit][..])
    );
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    g.state.pins.current = Some(SHA.into());
    let r = handle(&mut pc, &mut g, dev(), INIT);
    assert!(r.ok, "{r:?}");
    assert_eq!(
        steps(&pc).get(..2),
        Some(&[Call::Precheck, Call::Tuning][..])
    );
}

#[test]
fn a_job_begins_once_and_ends_by_its_run() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    let begin = Request::JobBegin { run: 7 };
    let r = handle(&mut pc, &mut g, begin.clone(), INIT);
    assert_eq!((r.ok, r.detail.as_str()), (true, "HIL job 7 began"));
    assert_eq!(g.state.job, Some(7));
    let r = handle(&mut pc, &mut g, Request::JobBegin { run: 8 }, INIT);
    assert_eq!(r.detail, "HIL job 7 has not ended");
    // job-end
    let r = handle(&mut pc, &mut g, Request::JobEnd { run: 8 }, INIT);
    assert_eq!((r.ok, r.detail.as_str()), (false, "HIL job 7 runs, not 8"));
    let r = handle(&mut pc, &mut g, Request::JobEnd { run: 7 }, INIT);
    assert_eq!((r.ok, r.detail.as_str()), (true, "HIL job 7 ended"));
    assert_eq!(g.state.job, None);
    let r = handle(&mut pc, &mut g, Request::JobEnd { run: 7 }, INIT);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (true, "no HIL job runs (job 7 has ended)")
    );
    // Only in dev.
    g.state.mode = Mode::Live;
    let r = handle(&mut pc, &mut g, begin, INIT);
    assert_eq!(r.detail, "a HIL job is for dev; the mode is live");
}

#[test]
fn the_test_signal_goes_only_to_the_hil_outputs() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    g.state.job = Some(7);
    let signal = |dbfs: f64, ttl_s: f64| Request::TestSignal {
        input: "mic1".into(),
        dbfs,
        ttl_s,
        listen: false,
    };
    let r = handle(&mut pc, &mut g, signal(-20.0, 5.0), INIT);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (
            true,
            "test signal on mic1 at -20 dBFS for 5 s on card outputs [94]"
        )
    );
    assert_eq!(
        pc.hil_signals,
        [("mic1".to_owned(), -20.0, 5.0, vec![94], false)]
    );
    for (dbfs, ttl, why) in [
        (
            -19.9,
            5.0,
            "-19.9 dBFS is above the HIL ceiling of -20 dBFS",
        ),
        (
            f64::NAN,
            5.0,
            "NaN dBFS is above the HIL ceiling of -20 dBFS",
        ),
        (-30.0, 0.0, "a TTL of 0 s is not a positive time"),
        (-30.0, -1.0, "a TTL of -1 s is not a positive time"),
        (
            -30.0,
            f64::INFINITY,
            "a TTL of inf s is not a positive time",
        ),
        (-30.0, f64::NAN, "a TTL of NaN s is not a positive time"),
    ] {
        let r = handle(&mut pc, &mut g, signal(dbfs, ttl), INIT);
        assert_eq!((r.ok, r.detail.as_str()), (false, why));
    }
    assert_eq!(pc.hil_signals.len(), 1);
    pc.fail(Call::HilSignal, "not a supervisor");
    let r = handle(&mut pc, &mut g, signal(-30.0, 1.0), INIT);
    assert_eq!((r.ok, r.detail.as_str()), (false, "not a supervisor"));
    g.site.hil_tx.clear();
    let r = handle(&mut pc, &mut g, signal(-30.0, 1.0), INIT);
    assert_eq!(
        r.detail,
        "[guard] hil_tx is empty: no card output may carry a test signal"
    );
    g.state.mode = Mode::Event;
    let r = handle(&mut pc, &mut g, signal(-30.0, 1.0), INIT);
    assert_eq!(r.detail, "test-signal is for dev; the mode is event");
    assert_eq!(HIL_MAX_DBFS, -20.0);
}

#[test]
fn a_test_signal_needs_a_begun_job_and_at_most_60_s() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    let signal = |ttl_s: f64| Request::TestSignal {
        input: "mic1".into(),
        dbfs: -30.0,
        ttl_s,
        listen: false,
    };
    // Without a begun job: refused.
    let r = handle(&mut pc, &mut g, signal(5.0), INIT);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "a test signal needs a begun HIL job (job-begin)")
    );
    assert!(!pc.called(Call::HilSignal));
    g.state.job = Some(7);
    let r = handle(&mut pc, &mut g, signal(60.0), INIT);
    assert!(r.ok, "{r:?}");
    let r = handle(&mut pc, &mut g, signal(60.5), INIT);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "a TTL of 60.5 s is above the HIL limit of 60 s")
    );
    assert_eq!(
        pc.hil_signals,
        [("mic1".to_owned(), -30.0, 60.0, vec![94], false)]
    );
    assert_eq!(HIL_MAX_TTL_S, 60.0);
}

#[test]
fn a_hil_report_records_the_result() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Dev));
    let report = |hil: &str| Request::Report {
        sha: SHA.into(),
        hil: hil.into(),
        detail: "120 s at 32".into(),
    };
    let r = handle(&mut pc, &mut g, report("green"), INIT);
    assert_eq!(r.detail, format!("bundle {SHA} is not installed"));
    g.state
        .bundles
        .insert(SHA.into(), record(SHA, "main", Hil::Pending));
    let r = handle(&mut pc, &mut g, report("green"), INIT);
    assert_eq!(
        (r.ok, r.detail.clone()),
        (true, format!("bundle {SHA}: HIL green (120 s at 32)"))
    );
    assert_eq!(g.state.bundles[SHA].hil, Hil::Green);
    handle(&mut pc, &mut g, report("red"), INIT);
    assert_eq!(g.state.bundles[SHA].hil, Hil::Red);
    let r = handle(&mut pc, &mut g, report("yellow"), INIT);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "HIL result \"yellow\": green or red")
    );
    assert_eq!(g.state.bundles[SHA].hil, Hil::Red);
}

#[test]
fn dev_only_requests_are_refused_elsewhere() {
    for (req, what) in [
        (Request::ForceReopen, "force-reopen"),
        (Request::InjectFault, "inject-fault"),
        (Request::InjectSeh, "inject-seh"),
        (Request::InjectPark, "inject-park"),
        (Request::RunnerStop, "runner-stop"),
        (Request::RehearseTeardown, "rehearse-teardown"),
        (
            Request::InstallSite {
                path: "site.toml".into(),
            },
            "install-site",
        ),
    ] {
        let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
        let r = handle(&mut pc, &mut g, req, INIT);
        assert_eq!(
            (r.ok, r.detail),
            (false, format!("{what} is for dev; the mode is event"))
        );
        assert!(pc.mutating_calls().is_empty(), "{what}");
    }
}

#[test]
fn engine_and_runner_requests_in_dev() {
    let (mut pc, mut g) = (
        FakePc::new(Facts {
            runner: true,
            ..iemmixer_up()
        }),
        Guard::for_test(Mode::Dev),
    );
    let r = handle(&mut pc, &mut g, Request::ForceReopen, INIT);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (true, "the engine reopened the driver")
    );
    g.state.job = Some(3);
    let r = handle(&mut pc, &mut g, Request::RunnerStop, INIT);
    assert_eq!(r.detail, "HIL job 3 runs: the runner is not idle");
    assert!(!pc.called(Call::RunnerStop));
    g.state.job = None;
    let r = handle(&mut pc, &mut g, Request::RunnerStop, INIT);
    assert_eq!((r.ok, r.detail.as_str()), (true, "the runner stopped"));
    pc.fail(Call::ForceReopen, "the reset budget is spent");
    let r = handle(&mut pc, &mut g, Request::ForceReopen, INIT);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "the reset budget is spent")
    );
    // The probe task runs in any mode.
    g.state.mode = Mode::Event;
    let r = handle(&mut pc, &mut g, Request::ProbeTask, INIT);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (true, "the probe task ended with 0")
    );
}

/// A rehearsal ends by entering dev again, and a dev entry cancels the jobs
/// and stops the runner: inside a HIL job that would stop the runner that
/// runs the job, so it is refused before any step, as runner-stop is.
#[test]
fn rehearse_teardown_is_refused_inside_a_hil_job() {
    let (mut pc, mut g) = (
        FakePc::new(Facts {
            runner: true,
            ..iemmixer_up()
        }),
        Guard::for_test(Mode::Dev),
    );
    g.state.pins.current = Some(SHA.into());
    g.state.job = Some(3);
    let r = handle(&mut pc, &mut g, Request::RehearseTeardown, INIT);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (
            false,
            "HIL job 3 runs: the rehearsal's dev entry would stop its runner"
        )
    );
    assert!(pc.mutating_calls().is_empty(), "{:?}", pc.mutating_calls());
    assert_eq!((g.state.job, g.state.mode), (Some(3), Mode::Dev));
    assert!(g.alarms.all().is_empty(), "{:?}", texts(&g));
    // After the job the rehearsal runs.
    g.state.job = None;
    let r = handle(&mut pc, &mut g, Request::RehearseTeardown, INIT);
    assert!(r.ok, "{r:?}");
    assert!(pc.called(Call::RunnerStop) && pc.called(Call::EngineStop));
}
