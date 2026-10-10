//! S7 additions to the commands (#10), additive both ways: an older engine
//! reads a newer request, and a newer engine reads an older one.

use serde::Deserialize;

use super::*;
use crate::ids::InputId;

/// `HilTestSignal` as an engine before S7 reads it: today's fields without
/// `listen`, and no `deny_unknown_fields`.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum OldCmd {
    HilTestSignal {
        input: InputId,
        hz: f64,
        dbfs: f64,
        ttl_s: f64,
        card_tx: Vec<u16>,
    },
}

const PLAIN: &str = r#"{"op":"hil_test_signal","input":"mic1","hz":1000.0,"dbfs":-20.0,"ttl_s":30.0,"card_tx":[94,95]}"#;
const PROBE: &str = r#"{"op":"hil_test_signal","input":"mic1","hz":1000.0,"dbfs":-20.0,"ttl_s":30.0,"card_tx":[94,95],"listen":true}"#;

fn hil(listen: bool) -> Cmd {
    Cmd::HilTestSignal {
        input: InputId::new("mic1"),
        hz: 1000.0,
        dbfs: -20.0,
        ttl_s: 30.0,
        card_tx: vec![94, 95],
        listen,
    }
}

/// The listen probe (S7 design note §6): `listen` is read as false when
/// missing and written only when true, so a plain HIL signal keeps today's
/// bytes; an older engine reads the probe as a plain HIL signal.
#[test]
fn the_listen_flag_of_the_hil_signal_is_additive() {
    assert_eq!(serde_json::from_str::<Cmd>(PLAIN).unwrap(), hil(false));
    assert_eq!(serde_json::from_str::<Cmd>(PROBE).unwrap(), hil(true));
    assert_eq!(serde_json::to_string(&hil(false)).unwrap(), PLAIN);
    assert_eq!(serde_json::to_string(&hil(true)).unwrap(), PROBE);
    let request = format!(r#"{{"type":"request","id":4,"cmd":{PROBE}}}"#);
    let ClientMsg::Request { id, cmd, .. } = parse_client(request.as_bytes()).unwrap() else {
        panic!("a request")
    };
    assert_eq!((id, cmd), (4, hil(true)));
    let old = OldCmd::HilTestSignal {
        input: InputId::new("mic1"),
        hz: 1000.0,
        dbfs: -20.0,
        ttl_s: 30.0,
        card_tx: vec![94, 95],
    };
    assert_eq!(serde_json::from_str::<OldCmd>(PROBE).unwrap(), old);
    assert_eq!(serde_json::from_str::<OldCmd>(PLAIN).unwrap(), old);
}

/// `Status` as a client before HIL v2 reads it: no reopen or fault time,
/// and no `deny_unknown_fields`.
#[derive(Debug, Deserialize)]
struct BeforeHilV2 {
    faulted: bool,
    resets: u64,
}

/// HIL v2 (S7, #10): the last driver reopen's time and the faulting
/// callback's time in `Status`, additive both ways: an older client reads
/// the new message, an older engine's reads 0 and 0.0. Both are written at
/// 0 too.
#[test]
fn the_reopen_and_fault_times_are_additive() {
    let status = Status {
        faulted: true,
        resets: 1,
        last_reopen_us: 104_000,
        fault_callback_us: 412.5,
        ..Status::default()
    };
    let json = serde_json::to_value(&status).unwrap();
    assert_eq!(json["last_reopen_us"], 104_000);
    assert_eq!(json["fault_callback_us"], 412.5);
    assert_eq!(
        serde_json::from_value::<Status>(json.clone()).unwrap(),
        status
    );
    let before: BeforeHilV2 = serde_json::from_value(json).unwrap();
    assert_eq!((before.faulted, before.resets), (true, 1));
    let old: Status = serde_json::from_str(r#"{"callbacks":4}"#).unwrap();
    assert_eq!(
        (old.callbacks, old.last_reopen_us, old.fault_callback_us),
        (4, 0, 0.0)
    );
    let zero = serde_json::to_value(Status::default()).unwrap();
    assert_eq!(zero["last_reopen_us"], 0);
    assert_eq!(zero["fault_callback_us"], 0.0);
}
