//! The listen probe through the guard (S7 part 4, #10): `iemmode test-signal
//! … --listen` reaches the engine as `HilTestSignal.listen`, behind the same
//! HIL job gate as the plain signal. New daemon tests live here, since
//! `daemon.rs` and `daemon/tests.rs` are over their size budget (#36).

use super::tests::iemmixer_up;
use super::*;
use crate::pc::fake::{Call, FakePc};
use crate::proto::Request;

/// The generation of a guard that began no switch and routed no "ide event".
const INIT: Generation = Generation { epoch: 0, fence: 0 };

fn signal(listen: bool) -> Request {
    Request::TestSignal {
        input: "mic1".into(),
        dbfs: -20.0,
        ttl_s: 5.0,
        listen,
    }
}

/// A guard in `mode` with `job` and a running iemmixer.
fn guard(mode: Mode, job: Option<u64>) -> (FakePc, Guard) {
    let mut g = Guard::for_test(mode);
    g.state.job = job;
    (FakePc::new(iemmixer_up()), g)
}

/// The reply to `req` and whether the engine was asked for a signal.
fn answer(mode: Mode, job: Option<u64>, req: Request) -> (bool, String, bool) {
    let (mut pc, mut g) = guard(mode, job);
    let r = handle(&mut pc, &mut g, req, INIT);
    (r.ok, r.detail, pc.called(Call::HilSignal))
}

#[test]
fn a_listen_probe_needs_a_begun_hil_job() {
    for (mode, job, why) in [
        (
            Mode::Dev,
            None,
            "a test signal needs a begun HIL job (job-begin)",
        ),
        (
            Mode::Event,
            Some(7),
            "test-signal is for dev; the mode is event",
        ),
        (
            Mode::Live,
            Some(7),
            "test-signal is for dev; the mode is live",
        ),
    ] {
        let probe = answer(mode, job, signal(true));
        assert_eq!(probe, (false, why.to_owned(), false), "{mode:?}");
        // Refused exactly as the plain signal.
        assert_eq!(answer(mode, job, signal(false)), probe, "{mode:?}");
    }
}

#[test]
fn a_listen_probe_reaches_the_engine_with_listen() {
    let (mut pc, mut g) = guard(Mode::Dev, Some(7));
    let r = handle(&mut pc, &mut g, signal(true), INIT);
    assert_eq!(
        (r.ok, r.detail.as_str()),
        (
            true,
            "test signal on mic1 at -20 dBFS for 5 s on card outputs [94]; listen probe"
        )
    );
    assert_eq!(
        pc.hil_signals,
        [("mic1".to_owned(), -20.0, 5.0, vec![94], true)]
    );
}

#[test]
fn a_plain_test_signal_sends_no_listen() {
    let (mut pc, mut g) = guard(Mode::Dev, Some(7));
    let r = handle(&mut pc, &mut g, signal(false), INIT);
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
}
