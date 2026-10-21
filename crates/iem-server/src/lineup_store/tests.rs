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

/// Writes `text` at `rel` under `dir`, its directories made.
fn put_in(dir: &Path, rel: &str, text: &str) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    put(&path, text);
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

/// #58 third review: a roster is kept under its SHA-256 in
/// `lineups/rosters.kept/`, and only a SHA-256 (64 lowercase hex digits) names
/// one: anything else is refused when kept and none when read.
#[test]
fn a_roster_is_kept_under_its_sha256_only() {
    let dir = tempfile::tempdir().unwrap();
    let s = LineupStore::new(dir.path());
    let sha = format!("0123456789abcdef{}", "a".repeat(48));
    assert_eq!(s.kept_roster(&sha).unwrap(), None, "none kept yet");
    s.keep_roster(&sha, "roster text").unwrap();
    assert_eq!(s.kept_roster(&sha).unwrap().as_deref(), Some("roster text"));
    assert_eq!(
        read(&dir.path().join(ROSTERS_DIR).join(format!("{sha}.toml"))),
        "roster text"
    );
    let upper = sha.to_uppercase();
    let short = &sha[..63];
    let long = format!("{sha}0");
    let other = format!("{}g", &sha[..63]);
    for bad in [
        upper.as_str(),
        short,
        long.as_str(),
        other.as_str(),
        "../active",
    ] {
        assert!(s.keep_roster(bad, "x").is_err(), "{bad} is kept");
        assert_eq!(s.kept_roster(bad).unwrap(), None, "{bad} is read");
    }
}

/// #58 fourth review: a roster kept already is not written again (the
/// start and every deferred poll keep the roster in use; a file of that
/// name is the same roster, its name being its SHA-256).
#[test]
fn a_kept_roster_is_not_written_again() {
    let dir = tempfile::tempdir().unwrap();
    let s = LineupStore::new(dir.path());
    let sha = "a".repeat(64);
    s.keep_roster(&sha, "first").unwrap();
    s.keep_roster(&sha, "second").unwrap();
    assert_eq!(s.kept_roster(&sha).unwrap().as_deref(), Some("first"));
}

/// #58 fourth review: the kept rosters' folder in `lineups/` is no
/// lineup's: no lineup id can name it, so no lineup shares it and no walk
/// over the lineups' folders (`people_archive`) takes it for one.
#[test]
fn the_kept_rosters_folder_is_named_by_no_lineup_id() {
    assert_eq!(ROSTERS_DIR, format!("{LINEUPS_DIR}/{ROSTERS_NAME}"));
    assert!(!valid_lineup_id(ROSTERS_NAME), "{ROSTERS_NAME}");
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
    // main → youth → main: the same store again, never a second one over
    // the same directory with a lock of its own.
    let main_store = state.band();
    state.roster.write().unwrap().as_mut().unwrap().active = "youth".into();
    assert!(Arc::ptr_eq(&store, &state.band()));
    state.roster.write().unwrap().as_mut().unwrap().active = "main".into();
    assert!(Arc::ptr_eq(&main_store, &state.band()));
    assert!(Arc::ptr_eq(&main_store, &state.band_of(Some("main"))));
    // An active lineup the roster lacks: main's, as the site view shows.
    state.roster.write().unwrap().as_mut().unwrap().active = "gone".into();
    assert!(Arc::ptr_eq(&main_store, &state.band()));
    assert!(!dir.path().join(LINEUPS_DIR).join("gone").exists());
    *state.roster.write().unwrap() = None;
    save(&state, "member3");
    assert!(
        dir.path()
            .join(file_of("customizations", "member3"))
            .exists()
    );
    assert!(Arc::ptr_eq(&state.band(), &state.band_of(None)));
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

/// #58: the engineer's presets, history and pins/hides are per lineup like
/// everyone's: they move into the main lineup and are never archived.
#[test]
fn the_engineers_files_move_into_the_main_lineup() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    legacy_files(d, "member1", &KINDS);
    legacy_files(d, ENGINEER_ID, &KINDS);
    let mut pins = pins(d, &[]);
    let report = migrate_legacy(d, &roster_without("member9"), &test_config(), &mut pins).unwrap();
    assert_eq!(report.moved, ["member1", ENGINEER_ID]);
    assert_eq!(report.archived, Vec::<String>::new());
    let main = d.join(LINEUPS_DIR).join("main");
    for kind in KINDS {
        assert_eq!(
            read(&main.join(file_of(kind, ENGINEER_ID))),
            format!("{kind} of engineer")
        );
        assert!(!d.join(file_of(kind, ENGINEER_ID)).exists());
    }
    assert!(!d.join("archive").exists());
    // A site without the engineer's [[members]] entry moves them too.
    let dir = tempfile::tempdir().unwrap();
    legacy_files(dir.path(), ENGINEER_ID, &["presets"]);
    let report = migrate_legacy(
        dir.path(),
        &roster_without("member9"),
        &Config::default(),
        &mut pins,
    )
    .unwrap();
    assert_eq!(report.moved, [ENGINEER_ID]);
}

/// A crash after the commit: the next call finishes the deletes and the
/// archives from `migration.json`, then removes it.
#[test]
fn a_crash_after_the_commit_is_finished_by_the_next_call() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    for id in ["member1", "member9"] {
        legacy_files(d, id, &KINDS);
    }
    photo(d, "member9");
    let mut pins = pins(d, &["member9"]);
    let roster = roster_without("member9");
    let crash = |step: Step| {
        if step == Step::Finish {
            Err(io::Error::other("power cut after the rename"))
        } else {
            Ok(())
        }
    };
    let err = migrate_with(d, &roster, &test_config(), &mut pins, &crash).unwrap_err();
    assert!(err.to_string().contains("power cut"), "{err}");
    // Committed: the main lineup plays; nothing deleted or archived yet.
    assert_eq!(
        LineupStore::new(d).active().unwrap().as_deref(),
        Some("main")
    );
    let pending: serde_json::Value = serde_json::from_str(&read(&d.join(PENDING_FILE))).unwrap();
    assert_eq!(pending["format"], PENDING_FORMAT);
    assert_eq!(pending["archive"], serde_json::json!(["member9"]));
    assert!(d.join(file_of("presets", "member1")).exists());
    assert_eq!(pins.member_hash("member9"), Some(PHC));
    // The next start finishes it.
    let report = migrate_legacy(d, &roster, &test_config(), &mut pins).unwrap();
    assert_eq!(
        report,
        MigrationReport {
            moved: vec![],
            taken_over: vec![],
            archived: vec!["member9".into()],
        }
    );
    for id in ["member1", "member9"] {
        for kind in KINDS {
            assert!(!d.join(file_of(kind, id)).exists(), "{kind}/{id}");
        }
    }
    assert_eq!(pins.archived_hash("member9"), Some(PHC));
    assert!(d.join("archive").join("member9").join("photo.jpg").exists());
    assert!(!d.join(PENDING_FILE).exists());
    let main = d.join(LINEUPS_DIR).join("main");
    assert_eq!(
        read(&main.join(file_of("presets", "member1"))),
        "presets of member1"
    );
    // And nothing more.
    assert_eq!(
        migrate_legacy(d, &roster, &test_config(), &mut pins).unwrap(),
        MigrationReport::default()
    );
}

/// Someone who is a person again by the time the pending work runs keeps
/// their files and PIN.
#[test]
fn a_former_member_who_is_back_is_not_archived_when_the_work_is_finished() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    legacy_files(d, "member9", &KINDS);
    let mut pins = pins(d, &["member9"]);
    let crash = |step: Step| {
        if step == Step::Finish {
            Err(io::Error::other("power cut after the rename"))
        } else {
            Ok(())
        }
    };
    let gone = roster_without("member9");
    migrate_with(d, &gone, &test_config(), &mut pins, &crash).unwrap_err();
    let back = (*test_roster("main").roster).clone();
    let report = migrate_legacy(d, &back, &test_config(), &mut pins).unwrap();
    assert_eq!(report.archived, Vec::<String>::new());
    assert_eq!(pins.member_hash("member9"), Some(PHC));
    let main = d.join(LINEUPS_DIR).join("main");
    assert_eq!(
        read(&main.join(file_of("presets", "member9"))),
        "presets of member9"
    );
    assert!(!d.join(file_of("presets", "member9")).exists());
    assert!(!d.join(PENDING_FILE).exists());
}

#[test]
fn a_roster_with_problems_or_a_takeover_of_a_person_migrates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    legacy_files(d, "member1", &KINDS);
    let mut pins = pins(d, &[]);
    let mut two_mains = (*test_roster("main").roster).clone();
    two_mains.lineups[1].main = true;
    let mut takes_a_person = (*test_roster("main").roster).clone();
    takes_a_person.people[9].takes_over = Some("member2".into());
    assert_eq!(takes_a_person.problems(), Vec::<String>::new());
    assert_eq!(
        migration_problems(&takes_a_person),
        ["person #10: takes_over names a person, whose files are their own"]
    );
    for roster in [two_mains, takes_a_person] {
        let err = migrate_legacy(d, &roster, &test_config(), &mut pins).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
        assert!(!d.join(LINEUPS_DIR).exists() && !d.join(STAGING_DIR).exists());
        assert!(d.join(file_of("presets", "member1")).exists());
    }
    assert_eq!(
        migration_problems(&test_roster("main").roster),
        Vec::<String>::new()
    );
}

#[test]
fn a_lineups_directory_without_its_active_file_is_never_committed_over() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    legacy_files(d, "member1", &KINDS);
    put_in(d, "lineups/youth/presets/member10.json", "someone's");
    let mut pins = pins(d, &[]);
    let err = migrate_legacy(d, &roster_without("member9"), &test_config(), &mut pins).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");
    assert!(err.to_string().contains("active.json"), "{err}");
    assert!(d.join(file_of("presets", "member1")).exists());
    assert!(!d.join(STAGING_DIR).exists());
    assert_eq!(
        read(&d.join("lineups/youth/presets/member10.json")),
        "someone's"
    );
}

#[test]
fn a_damaged_pending_file_is_an_error_and_deletes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    LineupStore::new(d).set_active("main", 1).unwrap();
    legacy_files(d, "member1", &["presets"]);
    let mut pins = pins(d, &[]);
    let roster = roster_without("member9");
    for text in [
        r#"{"format":"other","delete":["member1"],"archive":[]}"#,
        r#"{"format":"iemmixer-migration","delete":["../x"],"archive":[]}"#,
        r#"{"format":"iemmixer-migration","delete":[],"archive":["a b"]}"#,
        "{",
    ] {
        put_in(d, PENDING_FILE, text);
        let err = migrate_legacy(d, &roster, &test_config(), &mut pins).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{text}");
        assert!(d.join(file_of("presets", "member1")).exists(), "{text}");
    }
}

#[test]
fn only_a_transient_windows_error_is_tried_again() {
    let code = io::Error::from_raw_os_error;
    for c in [5, 32, 33] {
        assert!(transient(&code(c), true), "{c}");
        assert!(!transient(&code(c), false), "{c} off Windows");
    }
    for c in [2, 3, 17, 183] {
        assert!(!transient(&code(c), true), "{c}");
    }
    assert!(!transient(&io::Error::other("no code"), true));
}

#[test]
fn a_rename_is_tried_again_a_bounded_number_of_times() {
    use std::cell::Cell;
    let busy = || Err::<(), _>(io::Error::from_raw_os_error(32));
    let again = |e: &io::Error| e.raw_os_error() == Some(32);
    // Busy twice, then done.
    let (calls, pauses) = (Cell::new(0), Cell::new(0));
    let op = || {
        calls.set(calls.get() + 1);
        if calls.get() < 3 { busy() } else { Ok(()) }
    };
    retry(5, op, again, || pauses.set(pauses.get() + 1)).unwrap();
    assert_eq!((calls.get(), pauses.get()), (3, 2));
    // Busy for good: the last error after five tries.
    let (calls, pauses) = (Cell::new(0), Cell::new(0));
    let op = || {
        calls.set(calls.get() + 1);
        busy()
    };
    let err = retry(5, op, again, || pauses.set(pauses.get() + 1)).unwrap_err();
    assert_eq!(err.raw_os_error(), Some(32));
    assert_eq!((calls.get(), pauses.get()), (5, 4));
    // Any other error: at once.
    let calls = Cell::new(0);
    let op = || {
        calls.set(calls.get() + 1);
        Err::<(), _>(io::Error::from_raw_os_error(2))
    };
    assert!(retry(5, op, again, || {}).is_err());
    assert_eq!(calls.get(), 1);
}
