//! S7 additions to the commands (#10), additive both ways: an older engine
//! reads a newer request, and a newer engine reads an older one.

use super::*;

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
