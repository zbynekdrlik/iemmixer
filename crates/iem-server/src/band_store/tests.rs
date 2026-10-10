//! The band store's tests: presets, history, pins and hides, capture and the ramp.

use super::*;
use crate::site_view::tests::test_view;
use iem_engine_proto::{EngineMsg, Level, MixGroup, MixState, Transient};

fn store() -> (tempfile::TempDir, BandStore) {
    let dir = tempfile::tempdir().unwrap();
    let s = BandStore::new(dir.path());
    (dir, s)
}

fn content(n: usize) -> Captured {
    Captured {
        sends: (0..n)
            .map(|i| MixSend {
                src: Source::Input(InputId::new(format!("mic{}", i + 1))),
                gain_db: -1.0,
                pan: 0.0,
                muted: false,
            })
            .collect(),
        ..Captured::default()
    }
}

#[test]
fn presets_are_schema_3_files_with_a_limit_and_overwrite() {
    let (dir, s) = store();
    assert!(s.presets("member1").unwrap().is_empty());
    let p = s
        .save_preset("member1", "rehearsal", content(2), 100)
        .unwrap();
    assert_eq!((p.created_at, p.updated_at, p.sends.len()), (100, 100, 2));
    let text = std::fs::read_to_string(dir.path().join("presets/member1.json")).unwrap();
    let file: PresetFile = serde_json::from_str(&text).unwrap();
    assert_eq!(
        (file.format.as_str(), file.schema, file.member.as_str()),
        (PRESETS_FORMAT, 3, "member1")
    );
    let again = s
        .save_preset("member1", "rehearsal", content(3), 200)
        .unwrap();
    assert_eq!(
        (again.created_at, again.updated_at, again.sends.len()),
        (100, 200, 3)
    );
    for i in 1..MAX_PRESETS {
        s.save_preset("member1", &format!("p{i}"), content(1), 300 + i as i64)
            .unwrap();
    }
    assert_eq!(s.presets("member1").unwrap().len(), MAX_PRESETS);
    assert_eq!(
        s.save_preset("member1", "one too many", content(1), 999),
        Err(StoreError::Full)
    );
    // Overwriting at the limit is fine; newest update first.
    s.save_preset("member1", "rehearsal", content(1), 1000)
        .unwrap();
    assert_eq!(s.presets("member1").unwrap()[0].name, "rehearsal");
    assert_eq!(s.preset("member1", "p3").unwrap().unwrap().updated_at, 303);
    assert_eq!(s.preset("member1", "nope").unwrap(), None);
    assert!(s.delete_preset("member1", "p3").unwrap());
    assert!(!s.delete_preset("member1", "p3").unwrap());
    assert!(
        s.presets("member2").unwrap().is_empty(),
        "members are separate"
    );
}

#[test]
fn archived_entries_are_read_only_and_foreign_files_are_refused() {
    let (dir, s) = store();
    std::fs::create_dir_all(dir.path().join("presets")).unwrap();
    let archived = Preset {
        name: "old".into(),
        archived: true,
        ..Preset::default()
    };
    std::fs::write(
        dir.path().join("presets/member1.json"),
        serde_json::to_string(&PresetFile::new("member1", vec![archived])).unwrap(),
    )
    .unwrap();
    assert_eq!(
        s.save_preset("member1", "old", content(1), 5),
        Err(StoreError::Archived)
    );
    assert_eq!(s.delete_preset("member1", "old"), Err(StoreError::Archived));
    assert!(s.preset("member1", "old").unwrap().unwrap().archived);
    // A predecessor (legacy) file is refused, not overwritten.
    let legacy =
        r#"{"rehearsal":{"name":"rehearsal","channels":{},"created_at":1,"updated_at":2}}"#;
    std::fs::write(dir.path().join("presets/member2.json"), legacy).unwrap();
    assert!(matches!(s.presets("member2"), Err(StoreError::Corrupt(..))));
    assert!(matches!(
        s.save_preset("member2", "x", content(1), 1),
        Err(StoreError::Corrupt(..))
    ));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("presets/member2.json")).unwrap(),
        legacy
    );
    let wrong_schema = r#"{"format":"iemmixer-presets","schema":2,"member":"m","presets":[]}"#;
    std::fs::write(dir.path().join("presets/member3.json"), wrong_schema).unwrap();
    assert!(matches!(s.presets("member3"), Err(StoreError::Corrupt(..))));
    assert!(matches!(s.presets("../x"), Err(StoreError::BadMember(_))));
}

#[test]
fn history_prunes_the_oldest_unpinned_and_pins_with_a_label() {
    let (_dir, s) = store();
    let snap = |ts: i64, pinned: bool| Snapshot {
        timestamp: ts,
        label: "manual".into(),
        pinned,
        ..Snapshot::default()
    };
    s.add_snapshot("member1", snap(1, true)).unwrap();
    for ts in 2..=MAX_SNAPSHOTS as i64 + 5 {
        s.add_snapshot("member1", snap(ts, false)).unwrap();
    }
    let list = s.snapshots("member1").unwrap();
    assert_eq!(list.len(), MAX_SNAPSHOTS);
    assert_eq!(list[0].timestamp, MAX_SNAPSHOTS as i64 + 5, "newest first");
    assert!(list.iter().any(|x| x.timestamp == 1), "pinned kept");
    assert!(
        !list.iter().any(|x| x.timestamp == 2),
        "oldest unpinned gone"
    );
    assert!(
        s.pin_snapshot("member1", 10, true, Some("gig".into()))
            .unwrap()
    );
    let ten = s.snapshot("member1", 10).unwrap().unwrap();
    assert!(ten.pinned && ten.label == "gig");
    assert!(s.pin_snapshot("member1", 10, false, None).unwrap());
    assert!(!s.snapshot("member1", 10).unwrap().unwrap().pinned);
    assert!(!s.pin_snapshot("member1", 12345, true, None).unwrap());
    assert!(s.delete_snapshot("member1", 10).unwrap());
    assert_eq!(s.snapshot("member1", 10).unwrap(), None);
    // All pinned: nothing can be pruned.
    let mut all: Vec<Snapshot> = (0..MAX_SNAPSHOTS as i64 + 2)
        .map(|t| snap(t, true))
        .collect();
    prune(&mut all);
    assert_eq!(all.len(), MAX_SNAPSHOTS + 2);
}

/// A history entry's timestamp is its id (restore, pin, delete), so two
/// entries of one second — the day's auto-snapshot and a manual save, a
/// double tap on "Uložiť teraz" — must not share it: E2E run 36371298924
/// saved two entries of member6 as 1790564273, and pinning, restoring or
/// deleting the newer one reached the older.
#[test]
fn entries_of_one_second_keep_their_own_ids() {
    let (_dir, s) = store();
    let at = |label: &str| Snapshot {
        timestamp: 1_700_000_000,
        label: label.into(),
        ..Snapshot::default()
    };
    s.add_snapshot("member1", at(AUTO_LABEL)).unwrap();
    s.add_snapshot("member1", at("manual")).unwrap();
    s.add_snapshot("member1", at("tap")).unwrap();
    let ids = |s: &BandStore| {
        s.snapshots("member1")
            .unwrap()
            .into_iter()
            .map(|x| (x.timestamp, x.label, x.pinned))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ids(&s),
        vec![
            (1_700_000_002, "tap".to_string(), false),
            (1_700_000_001, "manual".to_string(), false),
            (1_700_000_000, AUTO_LABEL.to_string(), false),
        ],
        "each entry gets the next free second, newest first"
    );
    // Each id reaches its own entry.
    assert!(
        s.pin_snapshot("member1", 1_700_000_001, true, None)
            .unwrap()
    );
    assert!(s.delete_snapshot("member1", 1_700_000_000).unwrap());
    assert_eq!(
        ids(&s),
        vec![
            (1_700_000_002, "tap".to_string(), false),
            (1_700_000_001, "manual".to_string(), true),
        ]
    );
}

#[test]
fn the_daily_auto_snapshot_is_found_by_its_utc_day() {
    let (_dir, s) = store();
    let day = utc_day(1_700_000_000);
    assert_eq!(day, "2023-11-14");
    assert!(!s.has_auto_snapshot_on("member1", &day).unwrap());
    s.add_snapshot(
        "member1",
        Snapshot {
            timestamp: 1_700_000_000,
            label: "manual".into(),
            ..Snapshot::default()
        },
    )
    .unwrap();
    assert!(!s.has_auto_snapshot_on("member1", &day).unwrap());
    s.add_snapshot(
        "member1",
        Snapshot {
            timestamp: 1_700_000_100,
            label: AUTO_LABEL.into(),
            ..Snapshot::default()
        },
    )
    .unwrap();
    assert!(s.has_auto_snapshot_on("member1", &day).unwrap());
    assert!(!s.has_auto_snapshot_on("member1", "2023-11-15").unwrap());
}

#[test]
fn customizations_round_trip() {
    let (_dir, s) = store();
    assert!(s.customization("member1").unwrap().pinned.is_empty());
    s.save_customization(
        "member1",
        vec![Source::Input(InputId::new("mic1"))],
        vec![Source::Mix(MixId::new("member2"))],
    )
    .unwrap();
    let c = s.customization("member1").unwrap();
    assert_eq!(c.format, CUSTOMIZATION_FORMAT);
    assert_eq!(c.pinned, vec![Source::Input(InputId::new("mic1"))]);
    assert_eq!(c.hidden, vec![Source::Mix(MixId::new("member2"))]);
}

#[test]
fn customizations_are_per_member_and_a_save_replaces_them() {
    let (_dir, s) = store();
    let mic = |n: u32| Source::Input(InputId::new(format!("mic{n}")));
    s.save_customization("member1", vec![mic(1)], vec![])
        .unwrap();
    s.save_customization("member2", vec![], vec![mic(5)])
        .unwrap();
    let one = s.customization("member1").unwrap();
    let two = s.customization("member2").unwrap();
    assert_eq!((one.pinned, one.hidden), (vec![mic(1)], vec![]));
    assert_eq!((two.pinned, two.hidden), (vec![], vec![mic(5)]));
    // A save is the member's whole list, not an addition to it.
    s.save_customization("member1", vec![mic(5)], vec![])
        .unwrap();
    let one = s.customization("member1").unwrap();
    assert_eq!((one.pinned, one.hidden), (vec![mic(5)], vec![]));
    s.save_customization("member1", vec![], vec![]).unwrap();
    let one = s.customization("member1").unwrap();
    assert!(one.pinned.is_empty() && one.hidden.is_empty());
    assert_eq!(one.member, "member1");
    assert_eq!(
        s.customization("member2").unwrap().hidden,
        vec![mic(5)],
        "the other member's file is untouched"
    );
}

#[test]
fn a_missing_file_reads_as_its_members_empty_file() {
    let (_dir, s) = store();
    assert_eq!(
        s.customization("member1").unwrap(),
        CustomizationFile::new("member1", Vec::new(), Vec::new())
    );
}

#[test]
fn an_unreadable_file_is_an_error_not_an_empty_one() {
    let (dir, s) = store();
    // A folder where the file should be: reading it fails, not "not found".
    std::fs::create_dir_all(dir.path().join("customizations/member1.json")).unwrap();
    assert!(matches!(s.customization("member1"), Err(StoreError::Io(_))));
}

fn mirror(f: impl FnOnce(&mut MixState)) -> Mirror {
    let mut state = MixState::default();
    f(&mut state);
    let mut m = Mirror::default();
    m.apply(&EngineMsg::State {
        rev: 1,
        state,
        transient: Transient::default(),
    });
    m
}

#[test]
fn capture_takes_the_page_mix_and_the_members_input_eq() {
    let v = test_view();
    let m = mirror(|s| {
        let mx = s.mixes.entry(MixId::new("member1")).or_default();
        mx.inputs.insert(
            InputId::new("keys"),
            Level {
                gain_db: -7.0,
                pan: 0.25,
                muted: true,
            },
        );
        mx.groups.insert(
            GroupId::new("stems"),
            MixGroup {
                gain_db: -2.0,
                ..MixGroup::default()
            },
        );
        s.inputs.entry(InputId::new("mic1")).or_default().eq.gain_db = 1.5;
    });
    let c = capture(&v, &m, &v.page("member1").unwrap());
    assert_eq!(c.sends.len(), 24 + 8);
    let keys = c
        .sends
        .iter()
        .find(|s| s.src == Source::Input(InputId::new("keys")))
        .unwrap();
    assert_eq!((keys.gain_db, keys.pan, keys.muted), (-7.0, 0.25, true));
    assert_eq!(c.sends[24].src, Source::Mix(MixId::new("member2")));
    assert_eq!(c.groups[&GroupId::new("stems")], -2.0);
    assert_eq!(c.input_eq.len(), 1);
    assert_eq!(c.input_eq[&InputId::new("mic1")].gain_db, 1.5);
    // The translator page has no member: no input EQ.
    assert!(
        capture(&v, &m, &v.page("translator").unwrap())
            .input_eq
            .is_empty()
    );
}

fn level_of(c: &Cmd) -> (f64, f64, Option<bool>) {
    match c {
        Cmd::SetLevel {
            gain_db: Some(g),
            pan: Some(p),
            muted,
            ..
        } => (*g, *p, *muted),
        other => panic!("{other:?}"),
    }
}

#[test]
fn ramp_interpolates_amplitudes_and_lands_on_the_target() {
    let v = test_view();
    let p = v.page("member1").unwrap();
    let m = mirror(|s| {
        let mx = s.mixes.entry(MixId::new("member1")).or_default();
        mx.inputs.insert(
            InputId::new("mic1"),
            Level {
                gain_db: 0.0,
                pan: -1.0,
                muted: false,
            },
        );
        mx.inputs.insert(
            InputId::new("mic2"),
            Level {
                gain_db: -6.0,
                pan: 0.0,
                muted: false,
            },
        );
    });
    let sends = vec![
        MixSend {
            src: Source::Input(InputId::new("mic1")),
            gain_db: -6.020599913279624,
            pan: 1.0,
            muted: false,
        },
        // Unchanged: not sent.
        MixSend {
            src: Source::Input(InputId::new("mic2")),
            gain_db: -6.0,
            pan: 0.0,
            muted: false,
        },
        // Unknown here: skipped.
        MixSend {
            src: Source::Input(InputId::new("gone")),
            gain_db: 0.0,
            pan: 0.0,
            muted: false,
        },
    ];
    let groups = BTreeMap::from([(GroupId::new("stems"), 6.0), (GroupId::new("other"), 0.0)]);
    let (steps, skipped) = ramp(&v, &m, &p, &sends, &groups);
    assert_eq!(skipped, 2);
    assert_eq!(steps.len(), RAMP_STEPS);
    for (k, step) in steps.iter().enumerate() {
        assert_eq!(step.len(), 2, "mic1 and the stems strip");
        let (g, pan, muted) = level_of(&step[0]);
        let f = (k + 1) as f64 / 5.0;
        let lin = 1.0 + (0.5 - 1.0) * f;
        assert!((g - 20.0 * lin.log10()).abs() < 1e-9, "step {k}: {g}");
        assert!((pan - (-1.0 + 2.0 * f)).abs() < 1e-12);
        assert_eq!(muted, None);
    }
    let (g, pan, _) = level_of(&steps[4][0]);
    assert_eq!((g, pan), (-6.020599913279624, 1.0), "exactly the target");
    assert_eq!(
        steps[4][1],
        Cmd::SetGroup {
            mix: MixId::new("member1"),
            group: GroupId::new("stems"),
            gain_db: Some(6.0),
            muted: None
        }
    );
}

#[test]
fn ramp_orders_mutes_so_nothing_jumps() {
    let v = test_view();
    let p = v.page("member2").unwrap();
    let m = mirror(|s| {
        let mx = s.mixes.entry(MixId::new("member2")).or_default();
        mx.inputs.insert(
            InputId::new("mic1"),
            Level {
                gain_db: 0.0,
                pan: 0.0,
                muted: false,
            },
        );
        mx.inputs.insert(
            InputId::new("mic2"),
            Level {
                gain_db: 0.0,
                pan: 0.0,
                muted: true,
            },
        );
    });
    let sends = vec![
        // Being muted, stored louder: fades out, muted last, gain settled after.
        MixSend {
            src: Source::Input(InputId::new("mic1")),
            gain_db: 6.0,
            pan: 0.0,
            muted: true,
        },
        // Being unmuted: unmuted first, fades in from silence.
        MixSend {
            src: Source::Input(InputId::new("mic2")),
            gain_db: 0.0,
            pan: 0.0,
            muted: false,
        },
    ];
    let (steps, skipped) = ramp(&v, &m, &p, &sends, &BTreeMap::new());
    assert_eq!(skipped, 0);
    assert_eq!(steps.len(), RAMP_STEPS + 1);
    let mic1: Vec<(f64, f64, Option<bool>)> = steps[..5].iter().map(|s| level_of(&s[0])).collect();
    assert_eq!(mic1[0].2, None);
    assert_eq!(mic1[4], (DB_OFF, 0.0, Some(true)), "silent when muted");
    assert!(mic1[3].0 < mic1[0].0, "fading out");
    let mic2: Vec<(f64, f64, Option<bool>)> = steps[..5].iter().map(|s| level_of(&s[1])).collect();
    assert_eq!(mic2[0].2, Some(false));
    assert!(
        (mic2[0].0 - 20.0 * 0.2f64.log10()).abs() < 1e-9,
        "starts quiet"
    );
    assert_eq!(mic2[4], (0.0, 0.0, None));
    assert_eq!(
        steps[5],
        vec![Cmd::SetLevel {
            mix: MixId::new("member2"),
            source: Source::Input(InputId::new("mic1")),
            gain_db: Some(6.0),
            pan: None,
            muted: None
        }]
    );
    // Nothing to change: no batch at all.
    let (none, _) = ramp(&v, &m, &p, &[], &BTreeMap::new());
    assert!(none.is_empty());
    assert_eq!(lin_db(0.0), DB_OFF);
    assert_eq!(lin_db(1.0), 0.0);
}

#[test]
fn the_stems_fader_ramps_through_linear_amplitudes() {
    let v = test_view();
    let p = v.page("member1").unwrap();
    let m = mirror(|s| {
        s.mixes
            .entry(MixId::new("member1"))
            .or_default()
            .groups
            .insert(
                GroupId::new("stems"),
                MixGroup {
                    gain_db: -6.0,
                    ..MixGroup::default()
                },
            );
    });
    let groups = BTreeMap::from([(GroupId::new("stems"), 6.0)]);
    let (steps, skipped) = ramp(&v, &m, &p, &[], &groups);
    assert_eq!((steps.len(), skipped), (RAMP_STEPS, 0));
    let (a, b) = (db_to_lin(-6.0), db_to_lin(6.0));
    for (k, step) in steps.iter().enumerate() {
        let f = (k + 1) as f64 / RAMP_STEPS as f64;
        let want = 20.0 * (a + (b - a) * f).log10();
        match step.as_slice() {
            [
                Cmd::SetGroup {
                    gain_db: Some(g),
                    muted: None,
                    ..
                },
            ] => assert!((g - want).abs() < 1e-9, "step {k}: {g} dB, not {want}"),
            other => panic!("{other:?}"),
        }
    }
}
