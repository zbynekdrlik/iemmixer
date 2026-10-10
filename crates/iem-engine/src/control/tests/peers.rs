use super::*;
use crate::pipe::{control_name, listen};
use iem_engine_proto::{InputId, MixId, MixState, read_frame};
use interprocess::local_socket::Stream;
use interprocess::local_socket::prelude::*;

const WAIT: Duration = Duration::from_secs(5);

/// The engine's end and the client's end of one connection.
pub(super) fn peer(dir: &std::path::Path) -> (Conn, Stream) {
    peer_named(dir, "ctl")
}

/// One connection through its own socket `<name>.sock`.
fn peer_named(dir: &std::path::Path, name: &str) -> (Conn, Stream) {
    let path = dir
        .join(format!("{name}.sock"))
        .to_string_lossy()
        .into_owned();
    let listener = listen(control_name(&path).unwrap()).unwrap();
    let client = Stream::connect(control_name(&path).unwrap()).unwrap();
    client.set_recv_timeout(Some(WAIT)).unwrap();
    let start = Instant::now();
    loop {
        match listener.accept() {
            Ok(s) => return (Conn::new(s), client),
            Err(e) => {
                assert!(start.elapsed() < WAIT, "accept: {e}");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

/// Reads every message until the engine closes the connection (or 5 s pass).
pub(super) fn reader(mut client: Stream) -> std::thread::JoinHandle<Vec<EngineMsg>> {
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut buf = Vec::new();
        while read_frame(&mut client, &mut buf).is_ok() {
            out.push(serde_json::from_slice(&buf).unwrap());
        }
        out
    })
}

fn frame(msg: &ClientMsg) -> CtlMsg {
    CtlMsg::Frame {
        id: 1,
        bytes: serde_json::to_vec(msg).unwrap(),
    }
}

pub(super) fn hello() -> CtlMsg {
    frame(&ClientMsg::Hello {
        proto: PROTO,
        role: Role::Control,
        client: "test".into(),
    })
}

fn request(id: u64, cmd: Cmd) -> CtlMsg {
    frame(&ClientMsg::Request {
        id,
        origin: None,
        cmd,
    })
}

/// A frame from connection `conn`.
fn frame_from(conn: u64, msg: &ClientMsg) -> CtlMsg {
    CtlMsg::Frame {
        id: conn,
        bytes: serde_json::to_vec(msg).unwrap(),
    }
}

fn hello_as(conn: u64, role: Role) -> CtlMsg {
    frame_from(
        conn,
        &ClientMsg::Hello {
            proto: PROTO,
            role,
            client: "test".into(),
        },
    )
}

fn request_from(conn: u64, id: u64, cmd: Cmd) -> CtlMsg {
    frame_from(
        conn,
        &ClientMsg::Request {
            id,
            origin: None,
            cmd,
        },
    )
}

/// The error code of every reply (None: accepted), by request id.
fn codes(msgs: &[EngineMsg]) -> BTreeMap<u64, Option<ErrCode>> {
    msgs.iter()
        .filter_map(|m| match m {
            EngineMsg::Reply(r) => Some((r.id, r.error.as_ref().map(|e| e.code))),
            _ => None,
        })
        .collect()
}

fn superseded(msgs: &[EngineMsg]) -> usize {
    msgs.iter()
        .filter(|m| matches!(m, EngineMsg::Superseded))
        .count()
}

/// The HIL signal on a spare output: never a mix's TX (the owner's
/// decision on #9 of 2026-09-28).
fn hil() -> Cmd {
    Cmd::HilTestSignal {
        input: InputId::new("mic1"),
        hz: 1000.0,
        dbfs: -30.0,
        ttl_s: 60.0,
        card_tx: vec![SPARE[1]],
        listen: false,
    }
}

fn set_mix() -> Cmd {
    Cmd::SetMix {
        mix: MixId::new("member1"),
        volume_db: Some(-3.0),
        muted: None,
    }
}

#[test]
fn the_supervisor_may_stop_save_arm_and_test_but_never_mix() {
    let mut r = rig_with(
        crate::core::Flags {
            test_signal: true,
            fault_injection: false,
        },
        true,
    );
    let (ctl, ctl_client) = peer_named(r.dir.path(), "ctl");
    let (sup, sup_client) = peer_named(r.dir.path(), "sup");
    let (obs, obs_client) = peer_named(r.dir.path(), "obs");
    let (ctl_got, sup_got, obs_got) = (reader(ctl_client), reader(sup_client), reader(obs_client));
    for (id, conn) in [(1, ctl), (2, sup), (3, obs)] {
        r.c.handle(CtlMsg::Connected { id, conn });
    }
    r.c.handle(hello_as(1, Role::Control));
    r.c.handle(hello_as(2, Role::Supervisor));
    r.c.handle(hello_as(3, Role::Observe));
    // The supervisor's own commands, the reads, the save and the test
    // signals (under their flags); never a mix change.
    r.c.handle(request_from(2, 10, hil()));
    assert!(r.c.test_deadline.is_some(), "the HIL signal ends by itself");
    assert!(r.c.status_msg(&StreamStats::default()).held);
    r.c.handle(request_from(2, 11, Cmd::Arm));
    assert!(!r.c.status_msg(&StreamStats::default()).held, "armed");
    let start = Cmd::StartTestSignal {
        input: InputId::new("mic2"),
        hz: 500.0,
        dbfs: -40.0,
        ttl_s: 1.0,
    };
    let supervisor: [(u64, Cmd, Option<ErrCode>); 11] = [
        (12, Cmd::Ping, None),
        (13, Cmd::GetState, None),
        (14, Cmd::GetTopology, None),
        (15, Cmd::SaveNow, None),
        // A running HIL signal refuses a plain one: stop it first.
        (16, Cmd::StopTestSignal, None),
        (17, start, None),
        // Fault injection reaches the core, which refuses it without
        // the flag: never `NotController`.
        (18, Cmd::InjectFault, Some(ErrCode::Forbidden)),
        (22, Cmd::InjectPark, Some(ErrCode::Forbidden)),
        (19, set_mix(), Some(ErrCode::NotController)),
        (
            20,
            Cmd::Batch {
                ops: vec![set_mix()],
            },
            Some(ErrCode::NotController),
        ),
        (
            21,
            Cmd::ImportState {
                state: MixState::default(),
                baseline: false,
            },
            Some(ErrCode::NotController),
        ),
    ];
    for (id, cmd, _) in &supervisor {
        r.c.handle(request_from(2, *id, cmd.clone()));
    }
    // The controller may not arm, start the HIL signal or inject a
    // fault (an engine keeps `--fault-injection` until it restarts,
    // after its HIL job too, so only the guard, which allows them
    // inside a job, sends them; review of PR #40, #35); an observer
    // may do none of it either.
    r.c.handle(request_from(1, 30, Cmd::Arm));
    r.c.handle(request_from(1, 31, hil()));
    r.c.handle(request_from(1, 32, set_mix()));
    r.c.handle(request_from(1, 33, Cmd::InjectFault));
    r.c.handle(request_from(1, 34, Cmd::InjectSeh));
    r.c.handle(request_from(1, 35, Cmd::InjectPark));
    r.c.handle(request_from(3, 40, Cmd::Arm));
    r.c.handle(request_from(3, 41, hil()));
    r.c.handle(request_from(3, 42, Cmd::InjectPark));
    // The supervisor reads the meters like everyone.
    r.meters.input_buffer_mut().seq = 5;
    r.meters.publish();
    assert!(r.c.tick(Instant::now()).is_none());
    r.c.handle(request_from(2, 50, Cmd::Shutdown));
    assert!(r.c.shutdown, "the supervisor stops the engine");
    drop(r);
    let sup = sup_got.join().unwrap();
    assert!(
        matches!(&sup[0], EngineMsg::Hello(h) if h.role == Role::Supervisor),
        "{:?}",
        sup[0]
    );
    let got = codes(&sup);
    assert_eq!((got[&10], got[&11], got[&50]), (None, None, None));
    for (id, cmd, want) in &supervisor {
        assert_eq!(got[id], *want, "{cmd:?}");
    }
    assert!(
        sup.iter()
            .any(|m| matches!(m, EngineMsg::Meters(f) if f.seq == 5)),
        "meters"
    );
    let ctl = codes(&ctl_got.join().unwrap());
    assert_eq!(
        (ctl[&30], ctl[&31], ctl[&32]),
        (
            Some(ErrCode::NotSupervisor),
            Some(ErrCode::NotSupervisor),
            None
        )
    );
    // Refused for its role before the core reads the flag.
    assert_eq!(
        (ctl[&33], ctl[&34], ctl[&35]),
        (
            Some(ErrCode::NotSupervisor),
            Some(ErrCode::NotSupervisor),
            Some(ErrCode::NotSupervisor)
        )
    );
    let obs = codes(&obs_got.join().unwrap());
    assert_eq!(
        (obs[&40], obs[&41], obs[&42]),
        (
            Some(ErrCode::NotController),
            Some(ErrCode::NotController),
            Some(ErrCode::NotController)
        )
    );
}

/// HIL's forced reopen (S6 design note §7): the supervisor's, under
/// the fault-injection flag, reaches the backend; the controller's is
/// refused, and so is one without the flag.
#[test]
fn a_forced_reopen_reaches_the_backend_from_the_supervisor_only() {
    for (fault_injection, reopens, sup_code) in
        [(true, REOPEN, None), (false, 0, Some(ErrCode::Forbidden))]
    {
        let mut r = rig_with(
            crate::core::Flags {
                test_signal: false,
                fault_injection,
            },
            false,
        );
        let (d, count) = scripted(None, false);
        r.c.driver = Some(d);
        let (ctl, ctl_client) = peer_named(r.dir.path(), "ctl");
        let (sup, sup_client) = peer_named(r.dir.path(), "sup");
        let (ctl_got, sup_got) = (reader(ctl_client), reader(sup_client));
        r.c.handle(CtlMsg::Connected { id: 1, conn: ctl });
        r.c.handle(CtlMsg::Connected { id: 2, conn: sup });
        r.c.handle(hello_as(1, Role::Control));
        r.c.handle(hello_as(2, Role::Supervisor));
        r.c.handle(request_from(1, 7, Cmd::ForceReopen));
        assert_eq!(count.load(Ordering::Relaxed), 0, "the controller's");
        r.c.handle(request_from(2, 8, Cmd::ForceReopen));
        assert_eq!(count.load(Ordering::Relaxed), reopens);
        drop(r);
        let ctl = codes(&ctl_got.join().unwrap());
        assert_eq!(ctl[&7], Some(ErrCode::NotSupervisor));
        let sup = codes(&sup_got.join().unwrap());
        assert_eq!(sup[&8], sup_code, "fault injection {fault_injection}");
    }
    // NullRt has no card: the request is answered, nothing reopens.
    assert!(!Idle.force_reopen());
}

#[test]
fn the_test_signals_stay_under_their_flag_for_the_supervisor() {
    let mut r = rig_with(crate::core::Flags::default(), false);
    let (sup, sup_client) = peer_named(r.dir.path(), "sup");
    let got = reader(sup_client);
    r.c.handle(CtlMsg::Connected { id: 2, conn: sup });
    r.c.handle(hello_as(2, Role::Supervisor));
    r.c.handle(request_from(2, 1, hil()));
    r.c.handle(request_from(
        2,
        2,
        Cmd::StartTestSignal {
            input: InputId::new("mic2"),
            hz: 500.0,
            dbfs: -40.0,
            ttl_s: 1.0,
        },
    ));
    assert_eq!(r.c.test_deadline, None);
    drop(r);
    let got = codes(&got.join().unwrap());
    assert_eq!(
        (got[&1], got[&2]),
        (Some(ErrCode::Forbidden), Some(ErrCode::Forbidden))
    );
}

#[test]
fn a_second_supervisor_replaces_the_first_and_leaves_the_controller() {
    let mut r = rig();
    let mut clients = Vec::new();
    for (id, name) in [(1, "c1"), (2, "s1"), (3, "s2"), (4, "c2")] {
        let (conn, client) = peer_named(r.dir.path(), name);
        clients.push(reader(client));
        r.c.handle(CtlMsg::Connected { id, conn });
    }
    r.c.handle(hello_as(1, Role::Control));
    r.c.handle(hello_as(2, Role::Supervisor));
    r.c.handle(hello_as(3, Role::Supervisor));
    assert!(!r.c.peers.contains_key(&2), "the first supervisor is gone");
    assert!(r.c.peers.contains_key(&3));
    assert_eq!(r.c.controller, Some(1), "the controller stays");
    r.c.handle(request_from(1, 5, set_mix()));
    r.c.handle(request_from(3, 6, Cmd::Arm));
    // A new controller leaves the supervisor alone.
    r.c.handle(hello_as(4, Role::Control));
    assert_eq!(r.c.controller, Some(4));
    assert!(r.c.peers.contains_key(&3), "the supervisor stays");
    assert!(!r.c.peers.contains_key(&1));
    r.c.handle(request_from(3, 7, Cmd::Ping));
    drop(r);
    let got: Vec<Vec<EngineMsg>> = clients.into_iter().map(|c| c.join().unwrap()).collect();
    let [c1, s1, s2, c2] = [&got[0], &got[1], &got[2], &got[3]];
    assert_eq!(superseded(s1), 1);
    assert_eq!(superseded(s2), 0);
    assert_eq!(superseded(c1), 1, "by the second controller only");
    assert_eq!(superseded(c2), 0);
    assert_eq!(codes(c1)[&5], None, "set before the new controller");
    let s2 = codes(s2);
    assert_eq!((s2[&6], s2[&7]), (None, None));
}

#[test]
fn nothing_after_a_shutdown_request_is_handled() {
    let r = rig();
    r.status.faded_out.store(true, Ordering::Release);
    let (conn, client) = peer(r.dir.path());
    let got = reader(client);
    let (tx, rx) = std::sync::mpsc::channel();
    for msg in [
        CtlMsg::Connected { id: 1, conn },
        hello(),
        request(2, Cmd::Shutdown),
        request(3, Cmd::Ping),
    ] {
        tx.send(msg).unwrap();
    }
    let (exit, _) = run_bounded(r.c, rx);
    assert_eq!(exit, Exit::Shutdown { faded: true });
    let replies: Vec<u64> = got
        .join()
        .unwrap()
        .iter()
        .filter_map(|m| match m {
            EngineMsg::Reply(rep) => Some(rep.id),
            _ => None,
        })
        .collect();
    assert_eq!(
        replies,
        vec![2],
        "the ping after the shutdown stays unanswered"
    );
}

/// A backend whose stop ends as scripted (#35).
struct Stops(StopOutcome);

impl Driver for Stops {
    fn stats(&self) -> StreamStats {
        StreamStats {
            running: true,
            ..StreamStats::default()
        }
    }

    fn stop(self: Box<Self>) -> StopOutcome {
        self.0
    }
}

/// The stream's end says what happened (#35): a stream that stayed
/// parked (a callback stuck in it, or the parked-engine test's hold)
/// released nothing, so the engine says `DriverParked`, never
/// `DriverReleased`; a released one says `DriverReleased`. Either is
/// the last word before the engine closes the connection, and the
/// run ends as a shutdown either way.
#[test]
fn a_stop_that_leaves_the_stream_parked_says_so_never_released() {
    let reason = || "shutdown".to_owned();
    for (outcome, end) in [
        (
            StopOutcome::Released,
            EngineMsg::DriverReleased { reason: reason() },
        ),
        (
            StopOutcome::Parked,
            EngineMsg::DriverParked { reason: reason() },
        ),
    ] {
        let mut r = rig();
        r.c.driver = Some(Box::new(Stops(outcome)));
        r.status.faded_out.store(true, Ordering::Release);
        let (conn, client) = peer(r.dir.path());
        let got = reader(client);
        r.c.handle(CtlMsg::Connected { id: 1, conn });
        r.c.handle(hello());
        r.c.handle(request(2, Cmd::Shutdown));
        assert_eq!(
            r.c.tick(Instant::now()),
            Some(Exit::Shutdown { faded: true }),
            "{outcome:?}"
        );
        assert!(r.c.driver.is_none(), "{outcome:?}: stopped");
        let msgs = got.join().unwrap();
        let ends: Vec<&EngineMsg> = msgs
            .iter()
            .filter(|m| {
                matches!(
                    m,
                    EngineMsg::DriverReleased { .. } | EngineMsg::DriverParked { .. }
                )
            })
            .collect();
        assert_eq!(ends, vec![&end], "{outcome:?}");
        assert_eq!(msgs.last(), Some(&end), "{outcome:?}: the last word");
    }
}

#[test]
fn stopping_the_test_signal_clears_its_deadline() {
    let mut r = rig();
    let (conn, client) = peer(r.dir.path());
    let _got = reader(client);
    r.c.handle(CtlMsg::Connected { id: 1, conn });
    r.c.handle(hello());
    r.c.handle(request(
        2,
        Cmd::StartTestSignal {
            input: InputId::new("mic1"),
            hz: 1000.0,
            dbfs: -30.0,
            ttl_s: 60.0,
        },
    ));
    assert!(r.c.test_deadline.is_some());
    r.c.handle(request(3, Cmd::StopTestSignal));
    assert_eq!(r.c.test_deadline, None);
    assert!(r.c.core.transient().test_signal.is_none());
}

#[test]
fn an_import_as_baseline_writes_the_baseline() {
    let mut r = rig();
    let (conn, client) = peer(r.dir.path());
    let _got = reader(client);
    r.c.handle(CtlMsg::Connected { id: 1, conn });
    r.c.handle(hello());
    let import = |baseline: bool| Cmd::ImportState {
        state: MixState::default(),
        baseline,
    };
    let baseline = r.dir.path().join("state/baseline.json");
    r.c.handle(request(2, import(false)));
    assert_eq!(r.c.core.rev(), 1);
    assert!(!baseline.exists(), "a plain import keeps the baseline");
    r.c.handle(request(3, import(true)));
    let saved = crate::persist::decode(&std::fs::read(&baseline).unwrap()).unwrap();
    assert_eq!((saved.rev, r.c.core.rev()), (2, 2));
    assert_eq!(saved.topology_hash, r.c.core.topology().hash);
}
