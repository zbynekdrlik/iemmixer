//! `iem-engine supervise` (Gen 3 phase H, #72; plan Task 3) against the
//! control loop: `Control::run` on a thread of its own, its connections
//! taken by the engine's own acceptor (`engine::accept_control`) on a Unix
//! socket, and the client's `run` connecting to that socket by its path, as
//! on the box. The loop is ended by a controller's `Shutdown`.

use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;

use iem_engine_proto::{ClientMsg, Cmd, InputId, PROTO, db_to_lin, write_frame};
use interprocess::local_socket::Stream;
use interprocess::local_socket::prelude::*;

use super::*;
use crate::pipe::{control_name, listen};
use crate::supervise::{Ask, Failure, Outcome, Supervise, run};

const WAIT: Duration = Duration::from_secs(5);

/// The loop serving its control socket.
struct Served {
    pipe: String,
    exit: Receiver<Exit>,
    stop: Arc<AtomicBool>,
    /// Kept so the loop never sees its channel disconnected.
    _tx: Sender<CtlMsg>,
}

/// `run`'s loop and acceptor (`engine.rs`) around `c` on `dir/engine.sock`.
fn serve(c: Control, dir: &std::path::Path) -> Served {
    let pipe = dir.join("engine.sock").to_string_lossy().into_owned();
    let listener = listen(control_name(&pipe).unwrap()).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let (done, exit) = std::sync::mpsc::channel();
    let _ = std::thread::spawn(move || {
        let _ = done.send(c.run(&rx));
    });
    let stop = Arc::new(AtomicBool::new(false));
    let (frames, halt) = (tx.clone(), Arc::clone(&stop));
    let _ = std::thread::spawn(move || crate::engine::accept_control(listener, frames, halt));
    Served {
        pipe,
        exit,
        stop,
        _tx: tx,
    }
}

impl Served {
    fn ask(&self, ask: Ask) -> Result<Outcome, Failure> {
        run(&Supervise {
            pipe: self.pipe.clone(),
            ask,
        })
    }

    /// A controller's `Shutdown` (the test's own connection): the loop's
    /// exit within 5 s, then the acceptor stops.
    fn shut_down(self) -> Exit {
        let ctl = Stream::connect(control_name(&self.pipe).unwrap()).unwrap();
        for msg in [
            ClientMsg::Hello {
                proto: PROTO,
                role: Role::Control,
                client: "test".into(),
            },
            ClientMsg::Request {
                id: 1,
                origin: None,
                cmd: Cmd::Shutdown,
            },
        ] {
            write_frame(&mut &ctl, &msg).unwrap();
        }
        let exit = self.exit.recv_timeout(WAIT).expect("the loop ends");
        self.stop.store(true, Ordering::Release);
        drop(ctl);
        exit
    }
}

fn burst(listen: bool) -> Ask {
    Ask::TestSignal {
        input: InputId::new("mic1"),
        dbfs: -30.0,
        seconds: 30,
        listen,
    }
}

/// With the test-signal flag (the box's HIL job), the client's burst is
/// accepted and reaches the processor as the HIL signal on both spare
/// outputs, at its level and length with its listen flag; `status` names
/// the spare outputs; `stop-test-signal` ends it.
#[test]
fn the_client_sounds_reads_and_stops_the_hil_signal_under_the_flag() {
    let Rig {
        c,
        mut ring,
        status,
        dir,
        meters: _meters,
    } = rig_with(
        crate::core::Flags {
            test_signal: true,
            fault_injection: false,
        },
        false,
    );
    status.faded_out.store(true, Ordering::Release);
    let e = serve(c, dir.path());
    assert_eq!(
        e.ask(burst(true)),
        Ok(Outcome::TestSignal {
            rev: 1,
            outputs: SPARE.len(),
            dbfs: -30.0,
            seconds: 30,
            listen: true,
        })
    );
    let Ok(Outcome::Status(st)) = e.ask(Ask::Status) else {
        panic!("no status")
    };
    let spare: Vec<u16> = st.hil.iter().map(|h| h.tx).collect();
    assert_eq!(spare, SPARE);
    assert!(!st.held, "{st:?}");
    assert_eq!(e.ask(Ask::StopTestSignal), Ok(Outcome::Stopped { rev: 2 }));
    assert_eq!(e.shut_down(), Exit::Shutdown { faded: true });
    let mut ops = Vec::new();
    while let Ok(cmd) = ring.pop() {
        ops.push(cmd.op);
    }
    let hil: Vec<RtOp> = ops
        .iter()
        .copied()
        .filter(|op| matches!(op, RtOp::HilTestSignal { .. } | RtOp::StopTestSignal))
        .collect();
    let [
        RtOp::HilTestSignal {
            hz,
            amp,
            ttl,
            mask,
            listen,
            ..
        },
        RtOp::StopTestSignal,
    ] = hil.as_slice()
    else {
        panic!("{ops:?}")
    };
    assert_eq!((*hz, *amp), (1000.0, db_to_lin(-30.0)));
    assert_eq!(*ttl, 30 * u64::from(crate::SAMPLE_RATE));
    assert_eq!(mask[..2], [true, true], "both spare outputs");
    assert!(mask[2..].iter().all(|b| !b), "{mask:?}");
    assert!(*listen);
}

/// With the fault-injection flag (the box HIL gate's job), the client's
/// `inject-fault` is accepted and reaches the processor as the RT panic;
/// without it (a live engine, the live gate's job) the engine refuses it and
/// nothing reaches the processor. The ring here has no processor behind it,
/// so nothing faults: the loop ends by its controller's `Shutdown`.
#[test]
fn the_client_injects_the_rt_panic_only_under_the_fault_injection_flag() {
    for fault_injection in [true, false] {
        let Rig {
            c,
            mut ring,
            status,
            dir,
            meters: _meters,
        } = rig_with(
            crate::core::Flags {
                test_signal: true,
                fault_injection,
            },
            false,
        );
        status.faded_out.store(true, Ordering::Release);
        let e = serve(c, dir.path());
        let got = e.ask(Ask::InjectFault);
        if fault_injection {
            assert_eq!(got, Ok(Outcome::Injected { rev: 0 }));
        } else {
            assert_eq!(got, Err(Failure::Refused("forbidden".into())));
        }
        assert_eq!(e.shut_down(), Exit::Shutdown { faded: true });
        let mut panics = 0;
        while let Ok(cmd) = ring.pop() {
            if matches!(cmd.op, RtOp::Panic) {
                panics += 1;
            }
        }
        assert_eq!(panics, usize::from(fault_injection), "{fault_injection}");
    }
}

/// Without the flag (a live engine) the engine opens no spare output and
/// refuses the client's burst; nothing reaches the processor.
#[test]
fn without_the_test_signal_flag_the_engine_refuses_the_clients_burst() {
    let Rig {
        c,
        mut ring,
        status,
        dir,
        meters: _meters,
    } = rig_with(crate::core::Flags::default(), false);
    status.faded_out.store(true, Ordering::Release);
    let e = serve(c, dir.path());
    assert_eq!(
        e.ask(burst(false)),
        Err(Failure::Refused("forbidden".into()))
    );
    let Ok(Outcome::Status(st)) = e.ask(Ask::Status) else {
        panic!("no status")
    };
    assert!(st.hil.is_empty(), "{st:?}");
    assert_eq!(e.shut_down(), Exit::Shutdown { faded: true });
    while let Ok(cmd) = ring.pop() {
        assert!(
            !matches!(cmd.op, RtOp::HilTestSignal { .. }),
            "{:?}",
            cmd.op
        );
    }
}
