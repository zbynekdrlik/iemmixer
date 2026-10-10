//! The scripted PC (`pc::fake`) behaves as the daemon's tests rely on.

use std::time::{Duration, Instant};

use super::fake::{Call, FakePc};
use super::tests::up;
use super::*;

#[test]
fn the_fake_reports_its_facts_and_processes() {
    let mut pc = FakePc::new(up());
    assert_eq!(pc.facts(), up());
    pc.exited.push((Kid::Engine, Some(70)));
    let p = pc.procs();
    assert_eq!((p.reaper, p.app), (vec![1], vec![1]));
    assert!(p.engine.is_empty() && p.server.is_empty());
    assert!(p.tray.is_empty() && p.runner.is_empty());
    assert_eq!(p.exited, [(Kid::Engine, Some(70))]);
    assert!(pc.procs().exited.is_empty());
    assert_eq!(pc.calls(), [Call::Facts, Call::Procs, Call::Procs]);
    assert_eq!(pc.count(Call::Procs), 2);
    assert!(pc.mutating_calls().is_empty());
}

#[test]
fn the_fake_starts_and_stops_like_the_pc() {
    let c = Cancel::default();
    let mut pc = FakePc::new(up());
    pc.app_stop(&c).unwrap();
    pc.reaper_save_quit(&c).unwrap();
    assert_eq!(pc.facts, Facts::default());
    assert!(pc.tuning("enter", &c).unwrap().starts_with("enter"));
    assert!(pc.data(Mode::Dev, &c).unwrap().starts_with("Dev"));
    let engine = pc.engine_start(true, false).unwrap();
    pc.engine_ready(10, &c).unwrap();
    pc.engine_arm().unwrap();
    let server = pc.server_start(Mode::Dev).unwrap();
    assert!(server > engine);
    pc.tray_start().unwrap();
    assert_eq!(pc.identity("a", &c), Ok(None));
    pc.runner_start().unwrap();
    let f = pc.facts;
    assert!(f.engine && f.server && f.tray && f.runner && !f.reaper && !f.app);

    pc.runner_stop(&c).unwrap();
    pc.engine_stop(&c).unwrap();
    pc.server_stop(&c).unwrap();
    pc.tray_stop(&c).unwrap();
    pc.facts.other_module_holder = true;
    pc.holder_gone(&c).unwrap();
    pc.reaper_start().unwrap();
    pc.reaper_facts(&c).unwrap();
    pc.app_start().unwrap();
    pc.app_answers(&c).unwrap();
    pc.fingerprint().unwrap();
    assert_eq!(pc.facts, up());
    assert_eq!(pc.index(Call::AppStop), 0);
    assert_eq!(pc.index(Call::ReaperSaveQuit), 1);
    assert!(pc.index(Call::ReaperStart) > pc.index(Call::EngineStop));
    assert_eq!(
        pc.calls_after(Call::AppStart),
        [Call::AppAnswers, Call::Fingerprint]
    );
    assert_eq!(pc.calls_after(Call::Fingerprint), Vec::<Call>::new());
    assert!(pc.mutating_calls().contains(&Call::EngineStart));
    assert!(!pc.mutating_calls().contains(&Call::EngineReady));
    assert!(!pc.mutating_calls().contains(&Call::Identity));
}

/// #10: a REAPER still ending shows so until the wait sees it gone
/// (unless it outlasts the wait); one that ends by itself does so at
/// the scripted call, once; a start may show late.
#[test]
fn the_fake_ends_and_starts_reaper_like_the_pc() {
    let c = Cancel::default();
    let running = ReaperProcs {
        running: 1,
        ending: 0,
    };
    let ending = ReaperProcs {
        running: 0,
        ending: 1,
    };
    let mut pc = FakePc::new(up());
    assert_eq!(pc.reaper_procs(), Ok(running));
    pc.reaper_ending = true;
    assert_eq!(pc.reaper_procs(), Ok(ending));
    pc.reaper_held = true;
    pc.reaper_await_end(&c).unwrap();
    assert_eq!(pc.reaper_procs(), Ok(ending));
    pc.reaper_held = false;
    pc.reaper_await_end(&c).unwrap();
    assert_eq!(pc.reaper_procs(), Ok(ReaperProcs::default()));
    assert!(!pc.facts.reaper && !pc.facts.reaper_holds_module);
    assert_eq!(pc.count(Call::ReaperAwaitEnd), 2);
    assert!(!Call::ReaperAwaitEnd.mutates());
    // The read is no call.
    assert!(pc.calls().iter().all(|call| *call == Call::ReaperAwaitEnd));
    pc.reaper_shows_late = true;
    pc.reaper_start().unwrap();
    assert_eq!(pc.reaper_procs(), Ok(ReaperProcs::default()));
    pc.reaper_shows_late = false;
    pc.reaper_start().unwrap();
    assert_eq!(pc.reaper_procs(), Ok(running));
    assert!(pc.facts.reaper_holds_module);
    pc.reaper_ending = true;
    pc.reaper_ends_at = Some(Call::Fingerprint);
    pc.fingerprint().unwrap();
    assert_eq!(pc.reaper_procs(), Ok(ReaperProcs::default()));
    assert!(!pc.facts.reaper_holds_module);
    // Once: a later call leaves a new REAPER alone.
    pc.reaper_start().unwrap();
    pc.fingerprint().unwrap();
    assert_eq!(pc.reaper_procs(), Ok(running));
    // A failed wait changes nothing.
    pc.reaper_ending = true;
    pc.fail(Call::ReaperAwaitEnd, "pre-empted");
    assert!(pc.reaper_await_end(&c).is_err());
    assert_eq!(pc.reaper_procs(), Ok(ending));
    // An unreadable process list fails the read.
    pc.reaper_procs_fail = Some("the process list: access denied".into());
    assert_eq!(
        pc.reaper_procs(),
        Err(StepError::failed("the process list: access denied"))
    );
}

#[test]
fn the_fake_answers_the_reads() {
    let mut pc = FakePc::new(Facts::default());
    assert_eq!(pc.pref_check().unwrap(), PrefSeen::Original(0));
    assert_eq!(pc.tuning_drift().unwrap(), None);
    assert_eq!(pc.engine_health().unwrap(), Health::Dead);
    pc.health(Health::Healthy);
    assert_eq!(pc.engine_health().unwrap(), Health::Healthy);
    pc.precheck(Mode::Dev, false).unwrap();
    pc.probe_task().unwrap();
    pc.notify(Audience::Alarm, "t", "b").unwrap();
    assert_eq!(
        pc.notices,
        [(Audience::Alarm, "t".to_owned(), "b".to_owned())]
    );
    pc.set_bundle(Some("abc"));
    assert_eq!(pc.bundle.as_deref(), Some("abc"));
    let saved = Children {
        engine: Some(Child {
            pid: 5,
            start_time: 6,
            image: "e.exe".into(),
        }),
        ..Children::default()
    };
    assert_eq!(pc.adopt(&saved), saved);
    assert_eq!(pc.children(), saved);
    assert!(pc.called(Call::Notify));
    assert!(!pc.called(Call::AppStop));
    pc.engine_ready(10, &Cancel::default()).unwrap();
    assert_eq!(pc.ready_secs, [10]);
    pc.engine_hil_signal("mic1", -30.0, 5.0, &[94], true)
        .unwrap();
    assert_eq!(
        pc.hil_signals,
        [("mic1".to_owned(), -30.0, 5.0, vec![94], true)]
    );
    pc.engine_force_reopen().unwrap();
    pc.engine_inject_fault().unwrap();
    pc.engine_inject_seh().unwrap();
    pc.engine_inject_park().unwrap();
    assert_eq!(
        pc.install_site("site.toml", &Cancel::default()).unwrap(),
        "site.toml: checked and installed"
    );
    assert_eq!(pc.sites, ["site.toml"]);
    pc.exclude("a", &["b".to_owned()]).unwrap();
    assert_eq!(pc.excluded, [("a".to_owned(), vec!["b".to_owned()])]);
    for c in [
        Call::HilSignal,
        Call::ForceReopen,
        Call::InjectFault,
        Call::InjectSeh,
        Call::InjectPark,
        Call::InstallSite,
        Call::Exclude,
    ] {
        assert!(pc.called(c) && c.mutates(), "{c:?}");
    }
    // The engine is seen only while one runs; the look is no call.
    let calls = pc.calls().len();
    assert_eq!(pc.engine_seen(), None);
    pc.facts.engine = true;
    assert_eq!(pc.engine_seen(), Some(pc.seen.clone()));
    assert!(pc.seen.pipe_private);
    assert_eq!(pc.seen.status.frames, 32);
    // An engine coming up (no hello and Status yet) is not seen.
    pc.engine_up = false;
    assert_eq!(pc.engine_seen(), None);
    pc.engine_up = true;
    assert_eq!(pc.calls().len(), calls);
    pc.fail(Call::InstallSite, "check-site exit 2");
    assert!(pc.install_site("bad.toml", &Cancel::default()).is_err());
    assert_eq!(pc.sites, ["site.toml"]);
    pc.fail(Call::HilSignal, "refused");
    assert!(
        pc.engine_hil_signal("mic2", -30.0, 5.0, &[94], false)
            .is_err()
    );
    assert_eq!(pc.hil_signals.len(), 1);
}

#[test]
fn a_failed_fake_call_changes_nothing() {
    let c = Cancel::default();
    let mut pc = FakePc::new(Facts {
        engine: true,
        ..Facts::default()
    });
    pc.fail(Call::EngineStop, "no DriverReleased within 10 s");
    assert_eq!(
        pc.engine_stop(&c),
        Err(StepError::Failed("no DriverReleased within 10 s".into()))
    );
    assert!(pc.facts.engine);
    assert!(pc.called(Call::EngineStop));
}

#[test]
fn a_blocked_fake_call_ends_within_a_slice_of_the_preemption() {
    let mut pc = FakePc::new(up());
    pc.block_until_cancel(Call::Data);
    let c = Cancel::default();
    let other = c.clone();
    let fired = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        other.preempt();
        Instant::now()
    });
    assert_eq!(pc.data(Mode::Dev, &c), Err(StepError::Preempted));
    let back = Instant::now();
    let at = fired.join().unwrap();
    assert!(back.duration_since(at) < Duration::from_millis(600));
    assert_eq!(pc.first_after(at), None);
    let before = back.checked_sub(Duration::from_secs(5)).unwrap();
    let (first, t) = pc.first_after(before).unwrap();
    assert_eq!(first, Call::Data);
    assert!(t < at);
    // A call without a token cannot block.
    pc.block_until_cancel(Call::EngineStart);
    assert!(matches!(
        pc.engine_start(false, false),
        Err(StepError::Failed(_))
    ));
    assert!(!pc.facts.engine);
}

#[test]
fn a_delayed_fake_call_finishes_even_when_preempted() {
    let mut pc = FakePc::new(Facts::default());
    pc.delay(Call::EngineStart, Duration::from_millis(200));
    let c = Cancel::default();
    c.preempt();
    let t = Instant::now();
    assert!(pc.engine_start(false, true).is_ok());
    assert!(t.elapsed() >= Duration::from_millis(200));
    assert!(pc.facts.engine);
    assert!(c.preempted());
}
