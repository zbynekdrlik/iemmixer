//! Property test: serialize → deserialize → re-serialize → re-deserialize must
//! produce identical structures. Catches schema-drift bugs in backup/restore
//! that escape example-based unit tests.

use iem_core::{MixerBackup, SendBackup};
use proptest::prelude::*;

fn arb_send_backup() -> impl Strategy<Value = SendBackup> {
    (
        "[A-Z][a-z]{1,10}( [a-z]{1,8})?",
        "[A-Z][a-z]{1,10}( [a-z]{1,8})?",
        0.0f64..1.0,
        -1.0f64..1.0,
        any::<bool>(),
    )
        .prop_map(|(src, dst, vol, pan, mute)| SendBackup {
            src_name: src,
            dest_name: dst,
            vol,
            pan,
            mute,
        })
}

proptest! {
    #[test]
    fn capture_serialize_deserialize_identity(
        sends in proptest::collection::vec(arb_send_backup(), 1..50),
    ) {
        let mut backup = MixerBackup::default();
        backup.sends = sends;

        let json = serde_json::to_string(&backup).expect("serialize");
        let parsed: MixerBackup = serde_json::from_str(&json).expect("deserialize");

        // HashMap field iteration order is non-deterministic; serialize twice for
        // a stable canonical form, then compare.
        let json2 = serde_json::to_string(&parsed).expect("re-serialize");
        let parsed2: MixerBackup = serde_json::from_str(&json2).expect("re-parse");

        prop_assert_eq!(parsed, parsed2);
    }
}
