//! S7 HIL v2 (#10, plan Task 28): `Status` carries the last driver reopen's
//! time and the faulting callback's time, and a fault sends one last
//! `Status` before its alarm and the driver's release, so the guard keeps
//! the faulting callback's time across the respawn.

use super::*;

/// A backend whose stream reports these figures; its stop releases.
#[cfg(unix)]
struct Streams(StreamStats);

#[cfg(unix)]
impl Driver for Streams {
    fn stats(&self) -> StreamStats {
        self.0.clone()
    }

    fn stop(self: Box<Self>) -> StopOutcome {
        StopOutcome::Released
    }
}

#[test]
fn status_carries_the_last_reopen() {
    let r = rig();
    let s = r.c.status_msg(&StreamStats {
        resets: 1,
        last_reopen_us: 104_250,
        ..StreamStats::default()
    });
    assert_eq!((s.resets, s.last_reopen_us), (1, 104_250));
    // No reopen yet (NullRt never has one).
    assert_eq!(r.c.status_msg(&StreamStats::default()).last_reopen_us, 0);
}

#[test]
fn status_carries_the_faulting_callbacks_time_in_microseconds() {
    let r = rig();
    let s = r.c.status_msg(&StreamStats {
        faulted: true,
        fault_callback_ns: 412_000,
        ..StreamStats::default()
    });
    assert_eq!((s.faulted, s.fault_callback_us), (true, 412.0));
    let s = r.c.status_msg(&StreamStats {
        faulted: true,
        fault_callback_ns: 1_500,
        ..StreamStats::default()
    });
    assert_eq!(s.fault_callback_us, 1.5);
    assert_eq!(
        r.c.status_msg(&StreamStats::default()).fault_callback_us,
        0.0
    );
}

/// The tick that sees the fault broadcasts a `Status` with the faulting
/// callback's time first, then the fault's save (`Saved`), the `Fault`
/// alarm and `DriverReleased`: the last word stays the driver's release.
#[cfg(unix)]
#[test]
fn a_fault_sends_a_last_status_with_the_faulting_callbacks_time() {
    use super::peers::{hello, peer, reader};
    let mut r = rig();
    r.c.driver = Some(Box::new(Streams(StreamStats {
        frames: 32,
        callbacks: 3,
        faulted: true,
        max_process_ns: 412_000,
        fault_callback_ns: 412_000,
        fault: Some("boom".into()),
        ..StreamStats::default()
    })));
    let (conn, client) = peer(r.dir.path());
    let got = reader(client);
    r.c.handle(CtlMsg::Connected { id: 1, conn });
    r.c.handle(hello());
    assert_eq!(r.c.tick(Instant::now()), Some(Exit::Fault("boom".into())));
    assert!(r.c.driver.is_none(), "released");
    drop(r);
    let msgs = got.join().unwrap();
    let statuses: Vec<&Status> = msgs
        .iter()
        .filter_map(|m| match m {
            EngineMsg::Status(s) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(statuses.len(), 1, "{msgs:?}");
    let last = statuses[0];
    assert!(last.faulted, "{last:?}");
    assert_eq!(
        (last.callbacks, last.fault_callback_us, last.process_max_us),
        (3, 412.0, 412.0)
    );
    // `fault()` saves first: the save's `Saved` comes between them.
    let tail = msgs.len().checked_sub(4).map(|at| &msgs[at..]);
    assert!(
        matches!(
            tail,
            Some([
                EngineMsg::Status(s),
                EngineMsg::Saved { .. },
                EngineMsg::Alarm(Alarm {
                    code: AlarmCode::Fault,
                    detail,
                }),
                EngineMsg::DriverReleased { reason },
            ]) if s == last && detail == "boom" && reason == "fault"
        ),
        "{msgs:?}"
    );
}
