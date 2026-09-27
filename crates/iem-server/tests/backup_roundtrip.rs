//! Property test (gen1 `tests/backup_roundtrip.rs`, report §5 W5): any v2
//! backup — a random mix state with every field of every strip, random pins
//! and hides — reads back exactly as it was written, through its JSON text
//! and through the server's backup store. Catches schema drift (a renamed,
//! skipped or defaulted field) that the example-based tests miss.

use std::collections::BTreeMap;

use iem_core::backup::MixerBackup;
use iem_core::band::CustomizationFile;
use iem_engine_proto::{
    BandKind, DB_OFF, Eq, EqBand, GroupId, InputId, InputState, Level, Limiter, Mix, MixGroup,
    MixId, MixOut, MixState, Source,
};
use iem_server::backup_store::BackupStore;
use proptest::collection::{btree_map, vec};
use proptest::prelude::*;

/// An id as the site file allows it.
fn id() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_.-]{0,11}"
}

/// A gain: off, or anywhere in the engine's range.
fn db() -> impl Strategy<Value = f64> {
    prop_oneof![Just(DB_OFF), -150.0f64..24.0]
}

fn band() -> impl Strategy<Value = EqBand> {
    let kind = prop_oneof![
        Just(BandKind::HighPass),
        Just(BandKind::LowShelf),
        Just(BandKind::Peak),
        Just(BandKind::HighShelf),
    ];
    (kind, any::<bool>(), 10.0f64..24_000.0, db(), 0.01f64..4.0).prop_map(
        |(kind, enabled, freq_hz, gain_db, bw_oct)| EqBand {
            kind,
            enabled,
            freq_hz,
            gain_db,
            bw_oct,
        },
    )
}

fn eq() -> impl Strategy<Value = Eq> {
    (db(), [band(), band(), band(), band(), band()])
        .prop_map(|(gain_db, bands)| Eq { gain_db, bands })
}

fn input() -> impl Strategy<Value = InputState> {
    (db(), any::<bool>(), any::<bool>(), eq()).prop_map(|(trim_db, processing, muted, eq)| {
        InputState {
            trim_db,
            processing,
            muted,
            eq,
        }
    })
}

fn level() -> impl Strategy<Value = Level> {
    (db(), -1.0f64..=1.0, any::<bool>()).prop_map(|(gain_db, pan, muted)| Level {
        gain_db,
        pan,
        muted,
    })
}

fn mix() -> impl Strategy<Value = Mix> {
    let out = (db(), any::<bool>(), eq(), any::<bool>(), -6.0f64..=0.0).prop_map(
        |(volume_db, muted, eq, enabled, limit_db)| MixOut {
            volume_db,
            muted,
            eq,
            limiter: Limiter { enabled, limit_db },
        },
    );
    let group = (db(), any::<bool>(), eq()).prop_map(|(gain_db, muted, eq)| MixGroup {
        gain_db,
        muted,
        eq,
    });
    (
        out,
        btree_map(id().prop_map(InputId), level(), 0..8),
        btree_map(id().prop_map(GroupId), group, 0..3),
        btree_map(id().prop_map(MixId), level(), 0..4),
    )
        .prop_map(|(out, inputs, groups, mixes)| Mix {
            out,
            inputs,
            groups,
            mixes,
        })
}

fn source() -> impl Strategy<Value = Source> {
    prop_oneof![
        id().prop_map(|s| Source::Input(InputId(s))),
        id().prop_map(|s| Source::Mix(MixId(s))),
    ]
}

fn backup() -> impl Strategy<Value = MixerBackup> {
    let state = (
        btree_map(id().prop_map(InputId), input(), 0..8),
        btree_map(id().prop_map(MixId), mix(), 1..6),
    )
        .prop_map(|(inputs, mixes)| MixState { inputs, mixes });
    let customizations = btree_map(id(), (vec(source(), 0..4), vec(source(), 0..4)), 0..4)
        .prop_map(|m| {
            m.into_iter()
                .map(|(member, (pinned, hidden))| {
                    let file = CustomizationFile::new(member.clone(), pinned, hidden);
                    (member, file)
                })
                .collect::<BTreeMap<_, _>>()
        });
    let timestamp =
        "20[0-9]{2}-[01][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-5][0-9](\\.[0-9]{3})?Z";
    (timestamp, any::<u64>(), state, customizations).prop_map(
        |(timestamp, rev, state, customizations)| {
            let mut b = MixerBackup::new(timestamp, rev, state);
            b.customizations = customizations;
            b
        },
    )
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn capture_serialize_deserialize_identity(b in backup()) {
        // Through the JSON text: the same value, and the same text again
        // (gen1's serialise → parse → serialise check).
        let json = serde_json::to_string(&b).unwrap();
        let parsed = MixerBackup::parse(&json).unwrap();
        prop_assert_eq!(&parsed, &b);
        prop_assert_eq!(serde_json::to_string(&parsed).unwrap(), json);

        // Through the server's store: saved, listed with its numbers, and
        // loaded exactly as it was saved.
        let dir = tempfile::tempdir().unwrap();
        let store = BackupStore::new(dir.path());
        let name = store.save(&b).unwrap();
        prop_assert_eq!(store.load(&name).unwrap(), b.clone());
        let listed = store.list();
        prop_assert_eq!(listed.len(), 1);
        prop_assert_eq!(&listed[0].filename, &name);
        prop_assert_eq!(&listed[0].timestamp, &b.timestamp);
        prop_assert_eq!(listed[0].send_count, b.level_count());
        prop_assert_eq!(listed[0].track_count, b.state.mixes.len());
    }
}
