//! What the guard reports, against `FakePc`: the rehearsal's verdict, the
//! alarm test, the status line, the texts cut to one frame and the alarm
//! notices.

use iem_win::spawn::Placement;

use super::tests::{INIT, SHA, ask, dev, iemmixer_up, steps, texts};
use super::*;
use crate::pc::fake::{Call, FakePc};
use crate::plan::{Facts, Health};
use crate::proto::Request;

#[test]
fn rehearse_teardown_never_starts_reaper() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    g.state.pins.current = Some(SHA.into());
    let r = handle(&mut pc, &mut g, Request::RehearseTeardown, INIT);
    assert!(r.ok, "{r:?}");
    assert!(
        r.detail.starts_with(
            "teardown clean: module unheld, preference original, ports 80/443 free; dev: done"
        ),
        "{}",
        r.detail
    );
    for c in [
        Call::ReaperStart,
        Call::ReaperSaveQuit,
        Call::ReaperFacts,
        Call::AppStart,
        Call::AppStop,
        Call::AppAnswers,
    ] {
        assert!(!pc.called(c), "{c:?}");
    }
    let order = steps(&pc);
    let first = |c: Call| order.iter().position(|x| *x == c).unwrap();
    assert!(first(Call::EngineStop) < first(Call::ServerStop));
    assert!(first(Call::ServerStop) < first(Call::TrayStop));
    assert!(first(Call::TrayStop) < first(Call::Tuning));
    assert!(first(Call::Tuning) < first(Call::PrefCheck));
    // The teardown's, the rehearsal's own check, and the dev re-entry's
    // right before the engine starts.
    assert_eq!(pc.count(Call::PrefCheck), 3);
    assert!(pc.index(Call::EngineStart) > pc.index(Call::EngineStop));
    assert_eq!(g.state.mode, Mode::Dev);
    assert!(g.alarms.all().is_empty());
    // Not a switch: only the re-entry into dev counts.
    assert_eq!(g.shared.view().epoch, 1);
}

#[test]
fn a_rehearsal_that_finds_problems_says_so() {
    // The preference needed a write: reported, alarmed, dev entered again.
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    g.state.pins.current = Some(SHA.into());
    pc.pref_attempts = 1;
    let r = ask(&mut pc, &mut g, Request::RehearseTeardown);
    assert!(!r.ok);
    assert!(
        r.detail
            .starts_with("teardown problems: the preference needed 1 writes; dev: done"),
        "{}",
        r.detail
    );
    assert_eq!(
        texts(&g),
        ["rehearsal: teardown problems: the preference needed 1 writes"]
    );
    assert!(pc.called(Call::EngineStart));
    // Ports 80/443 still held after the teardown: the predecessor could not
    // serve the band (#9 2026-09-28); an unreadable owner is named too.
    for (ports, named) in [
        (
            Ok((Some(4242), None)),
            "ports 80/443 are still held (80: 4242, 443: free)",
        ),
        (
            Ok((None, Some(77))),
            "ports 80/443 are still held (80: free, 443: 77)",
        ),
        (Err("no table".to_owned()), "ports 80/443: no table"),
    ] {
        let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
        g.state.pins.current = Some(SHA.into());
        pc.ports = ports;
        let r = ask(&mut pc, &mut g, Request::RehearseTeardown);
        assert!(!r.ok);
        assert!(
            r.detail
                .starts_with(&format!("teardown problems: {named}; dev: done")),
            "{}",
            r.detail
        );
        assert_eq!(
            texts(&g),
            [format!("rehearsal: teardown problems: {named}")]
        );
    }
    // A holder that stays and processes that stay are named; the re-entry
    // that then fails stops and asks the owner, never starting REAPER.
    let (mut pc, mut g) = (
        FakePc::new(Facts {
            other_module_holder: true,
            ..iemmixer_up()
        }),
        Guard::for_test(Mode::Dev),
    );
    g.state.pins.current = Some(SHA.into());
    pc.fail(Call::TrayStop, "the tray did not quit within 10 s");
    pc.fail(Call::PrefCheck, "the registry is locked");
    let r = ask(&mut pc, &mut g, Request::RehearseTeardown);
    assert!(!r.ok);
    assert!(
        r.detail.starts_with(
            "teardown problems: iemmixer processes still run; the driver module is held; \
             the preference: the registry is locked; dev: stopped; the owner decides; \
             the mode is dev"
        ),
        "{}",
        r.detail
    );
    assert_eq!(
        texts(&g)[0],
        "TrayStop: rehearsal: the tray did not quit within 10 s"
    );
    let last = g.alarms.last().unwrap();
    assert_eq!(
        last.text,
        "TrayStop: the tray did not quit within 10 s; the rehearsal never starts REAPER"
    );
    assert!(last.owner_question);
    assert!(!pc.called(Call::ReaperStart) && !pc.called(Call::AppStart));
    assert_eq!(g.state.mode, Mode::Dev);
    // The flag ends with the rehearsal: a failed dev entry unwinds again.
    let r = ask(&mut pc, &mut g, dev());
    assert!(!r.ok);
    assert!(pc.called(Call::ReaperStart));
    // A healthy engine that does not release stops the rehearsal.
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    pc.fail(Call::EngineStop, "no DriverReleased within 10 s");
    pc.health(Health::Healthy);
    let r = ask(&mut pc, &mut g, Request::RehearseTeardown);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (
            false,
            "rehearsal stopped at EngineStop: no DriverReleased within 10 s"
        )
    );
    assert!(!pc.called(Call::ServerStop) && !pc.called(Call::EngineStart));
    let a = g.alarms.last().unwrap();
    assert!(a.owner_question);
    assert_eq!(
        a.text,
        "EngineStop: rehearsal: no DriverReleased within 10 s; engine healthy, iemmixer keeps serving"
    );
    assert_eq!(g.state.mode, Mode::Dev);
    // A dead one too, asking the owner.
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    pc.fail(Call::EngineStop, "refused");
    let r = ask(&mut pc, &mut g, Request::RehearseTeardown);
    assert!(!r.ok);
    assert_eq!(
        g.alarms.last().unwrap().text,
        "EngineStop: rehearsal: refused; health Some(Dead)"
    );
    assert!(!pc.called(Call::ServerStop));
}

#[test]
fn alarm_test_ack_status_quit_and_subscribe() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    let r = handle(&mut pc, &mut g, Request::AlarmTest, INIT);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (true, "the test alarm reached the engineer's devices")
    );
    assert_eq!(
        pc.notices,
        [(
            Audience::Alarm,
            "iemmixer alarm".to_owned(),
            "alarm test (iemmode alarm-test)".to_owned()
        )]
    );
    assert_eq!(r.alarms.len(), 1);
    let id = r.alarms[0].id;
    let r = handle(&mut pc, &mut g, Request::AlarmAck { id }, INIT);
    assert_eq!(
        (r.ok, r.detail.clone()),
        (true, format!("alarm {id} acknowledged"))
    );
    assert!(r.alarms[0].acked);
    let r = handle(&mut pc, &mut g, Request::AlarmAck { id: 99 }, INIT);
    assert_eq!((r.ok, r.detail.as_str()), (false, "no alarm 99"));
    pc.fail(Call::Notify, "no device took the notice");
    let r = handle(&mut pc, &mut g, Request::AlarmTest, INIT);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (false, "the test alarm was not delivered")
    );
    let r = handle(&mut pc, &mut g, Request::Status, INIT);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (true, "mode event; no bundle; 1 unacknowledged alarms")
    );
    let r = handle(&mut pc, &mut g, Request::Subscribe, INIT);
    assert!(r.ok);
    assert!(!g.quit);
    let r = handle(&mut pc, &mut g, Request::Quit, INIT);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (true, "the guard stops; its children keep running")
    );
    assert!(g.quit);
}

#[test]
fn status_names_everything_that_waits() {
    let mut g = Guard::for_test(Mode::Dev);
    assert_eq!(status_text(&g), "mode dev; no bundle");
    g.state.pins.current = Some(SHA.into());
    g.state.job = Some(4242);
    g.raise(None, "one", false);
    g.raise(None, "two", false);
    assert_eq!(
        status_text(&g),
        format!("mode dev; bundle {SHA}; HIL job 4242; 2 unacknowledged alarms")
    );
    assert_eq!(g.shared.view().status, status_text(&g));
    assert_eq!(
        [Mode::Event, Mode::Dev, Mode::Live].map(mode_name),
        ["event", "dev", "live"]
    );
}

/// On the PC the guard's task job allows no breakaway (#9 2026-09-28): a
/// guard whose children stay in its job (one that does not end its
/// processes when it closes) names it in `iemmode status` and the tray's
/// view from its start on, for its whole life; any other reading of the
/// job names nothing (a refusal is each start's step error).
#[test]
fn a_guard_whose_children_stay_in_its_job_names_it() {
    const NOTE: &str = "children stay in the guard task's job (no breakaway)";
    let mut g = Guard::for_test(Mode::Dev);
    let mut pc = FakePc::new(iemmixer_up());
    pc.job = Ok(Placement::InJob);
    assert_eq!(start(&mut pc, &mut g, 0), None);
    assert_eq!(status_text(&g), format!("mode dev; no bundle; {NOTE}"));
    assert_eq!(g.shared.view().status, status_text(&g));
    g.raise(None, "one", false);
    assert_eq!(
        status_text(&g),
        format!("mode dev; no bundle; {NOTE}; 1 unacknowledged alarms")
    );
    for job in [
        Ok(Placement::Breakaway),
        Ok(Placement::NoJob),
        Ok(Placement::Refuse("the job ends its processes")),
        Err("the job could not be read".to_owned()),
    ] {
        let mut g = Guard::for_test(Mode::Dev);
        let mut pc = FakePc::new(iemmixer_up());
        pc.job = job.clone();
        assert_eq!(start(&mut pc, &mut g, 0), None, "{job:?}");
        assert_eq!(status_text(&g), "mode dev; no bundle", "{job:?}");
        assert_eq!(g.shared.view().status, status_text(&g), "{job:?}");
    }
}

#[test]
fn texts_are_cut_to_fit_one_frame() {
    assert_eq!(cut("abcdé", 4), "abcd");
    assert_eq!(cut("čšž", 3), "čšž");
    assert_eq!(cut("čšž", 2), "čš");
    let mut g = Guard::for_test(Mode::Event);
    g.raise(None, &"x".repeat(ALARM_CHARS + 10), false);
    assert_eq!(g.alarms.last().unwrap().text.len(), ALARM_CHARS);
    assert_eq!((ALARM_CHARS, DETAIL_CHARS), (600, 8000));
    let long = "y".repeat(DETAIL_CHARS + 1);
    assert_eq!(g.reply(true, &long).detail.len(), DETAIL_CHARS);
    assert_eq!(
        g.shared.view().reply(true, &long).detail.len(),
        DETAIL_CHARS
    );
}

#[test]
fn notices_go_to_the_engineers_devices_once() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    g.raise(None, "x", false);
    send_notices(&mut pc, &mut g);
    assert_eq!(
        pc.notices,
        [(Audience::Alarm, "iemmixer alarm".to_owned(), "x".to_owned())]
    );
    assert!(g.alarms.last().unwrap().notified);
    assert!(g.shared.view().alarms[0].notified);
    send_notices(&mut pc, &mut g);
    assert_eq!(pc.count(Call::Notify), 1);
    // A failed notice is tried once, not at every look.
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    pc.fail(Call::Notify, "no bundle is active");
    g.raise(None, "y", false);
    send_notices(&mut pc, &mut g);
    send_notices(&mut pc, &mut g);
    assert_eq!(pc.count(Call::Notify), 1);
    assert!(!g.alarms.last().unwrap().notified);
    // Alarms raised meanwhile are sent in order.
    g.raise(None, "z1", false);
    g.raise(None, "z2", true);
    let mut ok = FakePc::new(Facts::default());
    send_notices(&mut ok, &mut g);
    let sent: Vec<&str> = ok.notices.iter().map(|n| n.2.as_str()).collect();
    assert_eq!(sent, ["z1", "z2"]);
    let notified = g.shared.view().alarms.iter().filter(|a| a.notified).count();
    assert_eq!(notified, 2);
}

/// A guard that starts again (a hand-over, a restart) reads the alarm file
/// and tries the alarms that never reached a device: only the open ones. An
/// acknowledged alarm never goes to a phone (#9 2026-09-28: a new guard sent
/// six acknowledged alarms from the morning once a device existed).
#[test]
fn an_acknowledged_alarm_is_never_sent() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Dev));
    g.raise(None, "old, acknowledged", false);
    let old = g.alarms.last().unwrap().id;
    assert!(g.alarms.ack(old));
    g.raise(None, "old, open", false);
    send_notices(&mut pc, &mut g);
    let sent: Vec<&str> = pc.notices.iter().map(|n| n.2.as_str()).collect();
    assert_eq!(sent, ["old, open"]);
    assert!(!g.alarms.iter().find(|a| a.id == old).unwrap().notified);
}
