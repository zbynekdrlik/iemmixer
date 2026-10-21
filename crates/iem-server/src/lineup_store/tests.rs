//! The lineups' files and the migration of the legacy layout (#58).

use super::*;

use std::sync::Arc;

use iem_engine_proto::{InputId, MixState};

use crate::AppState;
use crate::roster_state::tests::test_roster;
use crate::site_view::tests::test_config;

/// A PHC string of the right kind (the store checks the prefix only).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2hoYXNo";

fn stored(trim_db: f64) -> MixerBackup {
    let mut state = MixState::default();
    state
        .inputs
        .entry(InputId::new("keys"))
        .or_default()
        .trim_db = trim_db;
    MixerBackup::new("2026-10-21T12:00:00Z".into(), 1, state)
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn put(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
}

/// Writes `id`'s legacy files, each holding its kind and the id.
fn legacy_files(dir: &Path, id: &str, kinds: &[&str]) {
    for kind in kinds {
        let path = dir.join(file_of(kind, id));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, format!("{kind} of {id}")).unwrap();
    }
}

fn photo(dir: &Path, id: &str) {
    fs::create_dir_all(dir.join("photos")).unwrap();
    fs::write(dir.join("photos").join(format!("{id}.jpg")), id).unwrap();
}

fn pins(dir: &Path, ids: &[&str]) -> PinStore {
    let mut pins = PinStore::load(&dir.join("secrets")).unwrap();
    for id in ids {
        pins.set_member_hash(id, PHC.into()).unwrap();
    }
    pins
}

/// The test roster without `left` (who left the band; their set stays
/// empty in main).
fn roster_without(left: &str) -> Roster {
    let mut roster = (*test_roster("main").roster).clone();
    roster.people.retain(|p| p.id != left);
    for l in &mut roster.lineups {
        l.sets.retain(|_, p| p != left);
        l.leaders.retain(|p| p != left);
        for i in l.inputs.values_mut() {
            if i.owner.as_deref() == Some(left) {
                i.owner = None;
            }
        }
    }
    assert_eq!(roster.problems(), Vec::<String>::new());
    roster
}

#[test]
fn active_state_and_intent_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let s = LineupStore::new(dir.path());
    assert_eq!(s.active().unwrap(), None);
    assert_eq!(s.intent().unwrap(), None);
    s.set_active("youth", 1_760_000_000).unwrap();
    assert_eq!(s.active().unwrap().as_deref(), Some("youth"));
    let written: serde_json::Value =
        serde_json::from_str(&read(&dir.path().join(ACTIVE_FILE))).unwrap();
    assert_eq!(
        written,
        serde_json::json!({"format": ACTIVE_FORMAT, "lineup": "youth", "since": 1_760_000_000})
    );
    s.set_active("main", 2).unwrap();
    assert_eq!(s.active().unwrap().as_deref(), Some("main"));

    let intent = SwitchIntent {
        from: "main".into(),
        to: "youth".into(),
    };
    s.set_intent(&intent).unwrap();
    assert_eq!(s.intent().unwrap(), Some(intent));
    assert_eq!(
        read(&dir.path().join(SWITCH_FILE)),
        r#"{"from":"main","to":"youth"}"#
    );
    s.clear_intent().unwrap();
    assert_eq!(s.intent().unwrap(), None);
    assert!(!dir.path().join(SWITCH_FILE).exists());
    s.clear_intent().unwrap();

    let b = stored(3.0);
    s.save_state("youth", &b).unwrap();
    assert_eq!(s.state("youth").unwrap(), Some(b.clone()));
    assert_eq!(
        s.data_dir("youth"),
        dir.path().join(LINEUPS_DIR).join("youth")
    );
    assert_eq!(
        MixerBackup::parse(&read(&s.data_dir("youth").join("state.json"))).unwrap(),
        b
    );
    s.save_state("youth", &stored(-1.0)).unwrap();
    assert_eq!(s.state("youth").unwrap(), Some(stored(-1.0)));
}

#[test]
fn ids_and_files_are_checked() {
    let dir = tempfile::tempdir().unwrap();
    let s = LineupStore::new(dir.path());
    let b = stored(0.0);
    let main = |bad: &str| SwitchIntent {
        from: "main".into(),
        to: bad.into(),
    };
    let rev = |bad: &str| SwitchIntent {
        from: bad.into(),
        to: "main".into(),
    };
    for bad in ["", "../x", "Main"] {
        let input = io::ErrorKind::InvalidInput;
        assert_eq!(s.set_active(bad, 0).unwrap_err().kind(), input, "{bad}");
        assert_eq!(s.state(bad).unwrap_err().kind(), input, "{bad}");
        assert_eq!(s.save_state(bad, &b).unwrap_err().kind(), input, "{bad}");
        assert_eq!(s.set_intent(&main(bad)).unwrap_err().kind(), input);
        assert_eq!(s.set_intent(&rev(bad)).unwrap_err().kind(), input);
    }
    assert!(!dir.path().join(LINEUPS_DIR).exists(), "nothing written");
    // A foreign or damaged file is an error, never "none".
    fs::create_dir_all(dir.path().join(LINEUPS_DIR)).unwrap();
    let active = dir.path().join(ACTIVE_FILE);
    for text in [
        r#"{"format":"other","lineup":"main","since":0}"#,
        r#"{"format":"iemmixer-active-lineup","lineup":"../x","since":0}"#,
        "{",
    ] {
        put(&active, text);
        assert_eq!(
            s.active().unwrap_err().kind(),
            io::ErrorKind::InvalidData,
            "{text}"
        );
    }
    let switch = dir.path().join(SWITCH_FILE);
    for text in [
        r#"{"from":"main","to":"../x"}"#,
        r#"{"from":"../x","to":"main"}"#,
        "nope",
    ] {
        put(&switch, text);
        assert_eq!(
            s.intent().unwrap_err().kind(),
            io::ErrorKind::InvalidData,
            "{text}"
        );
    }
    let state = s.data_dir("main").join("state.json");
    fs::create_dir_all(state.parent().unwrap()).unwrap();
    for text in [r#"{"format":"iemmixer-presets","version":2}"#, "{"] {
        put(&state, text);
        assert_eq!(
            s.state("main").unwrap_err().kind(),
            io::ErrorKind::InvalidData,
            "{text}"
        );
    }
}

#[test]
fn a_missing_state_reads_as_none() {
    let dir = tempfile::tempdir().unwrap();
    let s = LineupStore::new(dir.path());
    assert_eq!(s.state("youth").unwrap(), None);
    s.save_state("main", &stored(1.0)).unwrap();
    assert_eq!(s.state("youth").unwrap(), None, "only main has one");
    // A state that cannot be read is no missing one.
    fs::create_dir_all(s.data_dir("youth").join("state.json")).unwrap();
    assert!(s.state("youth").is_err());
}

/// #58: the band store is the active lineup's, one per lineup while it
/// plays; legacy mode keeps the config directory.
#[tokio::test]
async fn the_band_store_follows_the_active_lineup() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::new(test_config(), dir.path());
    let save = |state: &AppState, who: &str| {
        state
            .band()
            .save_customization(who, vec![], vec![])
            .unwrap();
    };
    save(&state, "member1");
    assert!(
        dir.path()
            .join(file_of("customizations", "member1"))
            .exists()
    );
    assert!(Arc::ptr_eq(&state.band(), &state.band()), "one store");
    *state.roster.write().unwrap() = Some(test_roster("youth"));
    save(&state, "member10");
    let youth = dir.path().join(LINEUPS_DIR).join("youth");
    assert!(youth.join(file_of("customizations", "member10")).exists());
    let store = state.band();
    assert!(Arc::ptr_eq(&store, &state.band()), "one store per lineup");
    state.roster.write().unwrap().as_mut().unwrap().active = "main".into();
    save(&state, "member2");
    let main = dir.path().join(LINEUPS_DIR).join("main");
    assert!(main.join(file_of("customizations", "member2")).exists());
    assert!(!Arc::ptr_eq(&store, &state.band()));
    *state.roster.write().unwrap() = None;
    save(&state, "member3");
    assert!(
        dir.path()
            .join(file_of("customizations", "member3"))
            .exists()
    );
}

#[test]
fn migration_moves_main_people_files_and_archives_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    for id in ["member1", "member2", "member9"] {
        legacy_files(d, id, &KINDS);
    }
    photo(d, "member1");
    photo(d, "member9");
    let mut pins = pins(d, &["member1", "member9"]);
    let roster = roster_without("member9");
    let report = migrate_legacy(d, &roster, &test_config(), &mut pins).unwrap();
    assert_eq!(
        report,
        MigrationReport {
            moved: vec!["member1".into(), "member2".into()],
            taken_over: vec![],
            archived: vec!["member9".into()],
        }
    );
    let main = d.join(LINEUPS_DIR).join("main");
    for id in ["member1", "member2"] {
        for kind in KINDS {
            assert_eq!(
                read(&main.join(file_of(kind, id))),
                format!("{kind} of {id}")
            );
            assert!(
                !d.join(file_of(kind, id)).exists(),
                "the legacy copy is gone"
            );
        }
    }
    // Who is no person is archived: files, photo and PIN hash.
    let archive = d.join("archive").join("member9");
    for kind in KINDS {
        let archived = archive
            .join(LINEUPS_DIR)
            .join("main")
            .join(file_of(kind, "member9"));
        assert_eq!(read(&archived), format!("{kind} of member9"));
        assert!(!main.join(file_of(kind, "member9")).exists());
        assert!(!d.join(file_of(kind, "member9")).exists());
    }
    assert_eq!(read(&archive.join("photo.jpg")), "member9");
    assert!(!d.join("photos").join("member9.jpg").exists());
    assert!(d.join("photos").join("member1.jpg").exists());
    assert_eq!(pins.member_hash("member9"), None);
    assert_eq!(pins.archived_hash("member9"), Some(PHC));
    assert_eq!(pins.member_hash("member1"), Some(PHC));
    // The main lineup plays; the staging is gone.
    assert_eq!(
        LineupStore::new(d).active().unwrap().as_deref(),
        Some("main")
    );
    assert!(!d.join(STAGING_DIR).exists());
    // Once only: with lineups/ there, nothing happens.
    legacy_files(d, "member2", &["presets"]);
    assert_eq!(
        migrate_legacy(d, &roster, &test_config(), &mut pins).unwrap(),
        MigrationReport::default()
    );
    assert!(d.join(file_of("presets", "member2")).exists(), "untouched");
}

#[test]
fn takes_over_moves_presets_history_and_pins_hides_but_not_pin_or_photo() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    legacy_files(d, "member9", &KINDS);
    photo(d, "member9");
    let mut pins = pins(d, &["member9"]);
    // member10 used member9's login until now: member9 left, member10 wears
    // the set and takes member9's files.
    let mut roster = roster_without("member9");
    let member10 = roster
        .people
        .iter_mut()
        .find(|p| p.id == "member10")
        .unwrap();
    member10.takes_over = Some("member9".into());
    let main = roster.lineups.iter_mut().find(|l| l.main).unwrap();
    main.sets.insert("member9".into(), "member10".into());
    let report = migrate_legacy(d, &roster, &test_config(), &mut pins).unwrap();
    assert_eq!(
        report,
        MigrationReport {
            moved: vec![],
            taken_over: vec![("member10".into(), "member9".into())],
            archived: vec!["member9".into()],
        }
    );
    let lineup = d.join(LINEUPS_DIR).join("main");
    for kind in KINDS {
        assert_eq!(
            read(&lineup.join(file_of(kind, "member10"))),
            format!("{kind} of member9")
        );
        assert!(!lineup.join(file_of(kind, "member9")).exists());
        assert!(!d.join(file_of(kind, "member9")).exists());
    }
    // Never the PIN or the photo: those are member9's, in the archive.
    assert_eq!(pins.member_hash("member10"), None);
    assert_eq!(pins.archived_hash("member9"), Some(PHC));
    assert!(!d.join("photos").join("member10.jpg").exists());
    let archive = d.join("archive").join("member9");
    assert_eq!(read(&archive.join("photo.jpg")), "member9");
    assert!(
        !archive.join(LINEUPS_DIR).exists(),
        "no file of theirs left"
    );
}

#[test]
fn a_taker_keeps_their_own_files_and_the_rest_is_archived() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    // member2 plays on; their own presets and history stay theirs. old7, a
    // login the site no longer lists, leaves presets and pins/hides.
    legacy_files(d, "member2", &["presets", "snapshots"]);
    legacy_files(d, "old7", &["presets", "customizations"]);
    let mut pins = pins(d, &[]);
    let mut roster = (*test_roster("main").roster).clone();
    let member2 = roster
        .people
        .iter_mut()
        .find(|p| p.id == "member2")
        .unwrap();
    member2.takes_over = Some("old7".into());
    assert_eq!(roster.problems(), Vec::<String>::new());
    let report = migrate_legacy(d, &roster, &test_config(), &mut pins).unwrap();
    assert_eq!(report.moved, ["member2"]);
    assert_eq!(
        report.taken_over,
        [("member2".to_string(), "old7".to_string())]
    );
    assert_eq!(report.archived, ["old7"]);
    let lineup = d.join(LINEUPS_DIR).join("main");
    let in_main = |kind: &str| read(&lineup.join(file_of(kind, "member2")));
    assert_eq!(in_main("presets"), "presets of member2");
    assert_eq!(in_main("snapshots"), "snapshots of member2");
    assert_eq!(in_main("customizations"), "customizations of old7");
    let archived = d
        .join("archive")
        .join("old7")
        .join(LINEUPS_DIR)
        .join("main")
        .join(file_of("presets", "old7"));
    assert_eq!(read(&archived), "presets of old7");
    for kind in KINDS {
        assert!(!d.join(file_of(kind, "old7")).exists());
        assert!(!lineup.join(file_of(kind, "old7")).exists());
    }
}

#[test]
fn a_crash_mid_migration_leaves_the_legacy_layout_and_runs_again() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    for id in ["member1", "member9"] {
        legacy_files(d, id, &KINDS);
    }
    photo(d, "member9");
    let mut pins = pins(d, &["member9"]);
    let roster = roster_without("member9");
    let crash = |step: Step| {
        if step == Step::Commit {
            Err(io::Error::other("power cut before the rename"))
        } else {
            Ok(())
        }
    };
    let err = migrate_with(d, &roster, &test_config(), &mut pins, &crash).unwrap_err();
    assert!(err.to_string().contains("power cut"), "{err}");
    // The legacy layout is whole: files, photo, PIN; no lineups/.
    for id in ["member1", "member9"] {
        for kind in KINDS {
            assert_eq!(read(&d.join(file_of(kind, id))), format!("{kind} of {id}"));
        }
    }
    assert!(d.join("photos").join("member9.jpg").exists());
    assert_eq!(pins.member_hash("member9"), Some(PHC));
    assert!(!d.join(LINEUPS_DIR).exists());
    assert!(!d.join("archive").exists());
    assert!(d.join(STAGING_DIR).exists(), "the half-built staging");
    fs::write(d.join(STAGING_DIR).join("junk"), "left over").unwrap();
    // The next start removes the staging and runs the whole migration.
    let report = migrate_legacy(d, &roster, &test_config(), &mut pins).unwrap();
    assert_eq!(report.moved, ["member1"]);
    assert_eq!(report.archived, ["member9"]);
    assert!(!d.join(STAGING_DIR).exists());
    assert!(!d.join(LINEUPS_DIR).join("junk").exists());
    let main = d.join(LINEUPS_DIR).join("main");
    assert_eq!(
        read(&main.join(file_of("presets", "member1"))),
        "presets of member1"
    );
    assert_eq!(pins.archived_hash("member9"), Some(PHC));
}
