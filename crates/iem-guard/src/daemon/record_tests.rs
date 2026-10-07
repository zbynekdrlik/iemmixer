//! The daemon's records in its replies (S7, #10): the engine's pid. New
//! daemon tests live here, since `daemon.rs` and `daemon/tests.rs` are over
//! their size budget (#36).

use super::*;
use crate::pc::fake::FakePc;
use crate::plan::Facts;
use crate::state::Child;

/// The generation of a guard that began no switch and routed no "ide event".
const INIT: Generation = Generation { epoch: 0, fence: 0 };

/// The soak's "one pid" (S7 design note §4): `Reply.engine` names the engine
/// process the guard started or adopted (`GuardState.pids`), and none while
/// it knows of none.
#[test]
fn the_engine_reply_names_its_pid() {
    let mut pc = FakePc::new(Facts {
        engine: true,
        ..Facts::default()
    });
    let mut g = Guard::for_test(Mode::Dev);
    g.state.pids.engine = Some(Child {
        pid: 4242,
        start_time: 1,
        image: "iem-engine.exe".into(),
    });
    let r = handle(&mut pc, &mut g, Request::Status, INIT);
    assert_eq!(r.engine.map(|e| e.pid), Some(Some(4242)));
    g.state.pids.engine = None;
    let r = handle(&mut pc, &mut g, Request::Status, INIT);
    assert_eq!(
        r.engine.map(|e| e.pid),
        Some(None),
        "an engine seen, no child"
    );
}
