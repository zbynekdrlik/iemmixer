use super::*;
use crate::persist::tests::{sample, store};
use crate::test_support::test_site;
use iem_engine_proto::{InputId, InputState};

fn corrupt(path: &Path) {
    let mut text = fs::read_to_string(path).unwrap();
    let at = text.find("\"rev\":").unwrap() + 6;
    text.insert(at, '9');
    fs::write(path, text).unwrap();
}

#[test]
fn live_state_sees_current_and_generations_independently() {
    let (_d, s) = store();
    // A fresh store holds no live state.
    assert_eq!(s.live_state().unwrap(), None);
    // `current.json` alone is live state.
    std::fs::write(s.dir().join(CURRENT), encode(&sample(1)).unwrap()).unwrap();
    assert_eq!(s.live_state().unwrap(), Some(Source::Current));
    // A generation ALONE, with no `current.json`, is also live state: a
    // crash between `save`'s two renames leaves the previous state only as
    // a generation (the newest waits in save.tmp: #32 D6).
    std::fs::remove_file(s.dir().join(CURRENT)).unwrap();
    std::fs::write(
        s.dir().join("gen-0000000001.json"),
        encode(&sample(1)).unwrap(),
    )
    .unwrap();
    assert_eq!(s.live_state().unwrap(), Some(Source::Generation(1)));
}

#[test]
fn an_interrupted_save_is_live_state() {
    // #32 D6: `save` writes the new state to save.tmp before its renames,
    // so a crash between them leaves the newest state only there.
    let (_d, s) = store();
    fs::write(s.dir().join(TMP), encode(&sample(2)).unwrap()).unwrap();
    assert_eq!(s.live_state().unwrap(), Some(Source::Interrupted));
}

#[test]
fn live_state_follows_the_load_chains_order() {
    // current.json, then save.tmp, then the newest generation.
    let (_d, s) = store();
    fs::write(
        s.dir().join("gen-0000000002.json"),
        encode(&sample(2)).unwrap(),
    )
    .unwrap();
    fs::write(
        s.dir().join("gen-0000000003.json"),
        encode(&sample(3)).unwrap(),
    )
    .unwrap();
    assert_eq!(s.live_state().unwrap(), Some(Source::Generation(3)));
    fs::write(s.dir().join(TMP), encode(&sample(9)).unwrap()).unwrap();
    assert_eq!(s.live_state().unwrap(), Some(Source::Interrupted));
    fs::write(s.dir().join(CURRENT), encode(&sample(10)).unwrap()).unwrap();
    assert_eq!(s.live_state().unwrap(), Some(Source::Current));
}

#[test]
fn live_state_fails_when_the_state_dir_cannot_be_read() {
    // #32 D5: an I/O error is not "no state" — the seed must never write
    // over state it could not look at.
    let (_d, s) = store();
    fs::remove_dir(s.dir()).unwrap();
    fs::write(s.dir(), b"not a directory").unwrap();
    assert!(s.live_state().is_err());
}

#[test]
fn a_bad_checksum_falls_back_to_the_newest_good_generation() {
    let (_d, s) = store();
    for rev in 1..=3 {
        s.save(&sample(rev)).unwrap();
    }
    corrupt(&s.dir().join(CURRENT));
    let loaded = s.load(&test_site());
    assert_eq!(loaded.source, Source::Generation(2));
    assert_eq!(loaded.persisted.rev, 2);
    assert_eq!(loaded.rejected.len(), 1);
    assert!(loaded.rejected[0].0.ends_with(CURRENT));
    assert_eq!(loaded.rejected[0].1, "checksum mismatch");
}

#[test]
fn truncated_or_foreign_files_are_skipped() {
    let (_d, s) = store();
    for rev in 1..=3 {
        s.save(&sample(rev)).unwrap();
    }
    let current = s.dir().join(CURRENT);
    let bytes = fs::read(&current).unwrap();
    fs::write(&current, &bytes[..bytes.len() / 2]).unwrap();
    let gens = s.generations().unwrap();
    let newest = &gens.last().unwrap().1;
    let text = fs::read_to_string(newest)
        .unwrap()
        .replace("iemmixer-state", "not-ours");
    fs::write(newest, text).unwrap();
    let loaded = s.load(&test_site());
    assert_eq!(loaded.source, Source::Generation(1));
    assert_eq!(loaded.persisted.rev, 1);
    assert_eq!(loaded.rejected.len(), 2);
}

#[test]
fn a_crash_between_the_renames_boots_on_the_interrupted_save() {
    // #32: `save` writes save.tmp, renames current.json to a generation,
    // then save.tmp to current.json. A crash between the renames leaves
    // the newest state only in save.tmp: the load chain must take it, not
    // the older generation (whose boot would lose it at the next save).
    let (_d, s) = store();
    let g = test_site();
    s.save(&sample(1)).unwrap();
    fs::write(s.dir().join(TMP), encode(&sample(2)).unwrap()).unwrap();
    fs::rename(s.dir().join(CURRENT), s.dir().join("gen-0000000001.json")).unwrap();
    let loaded = s.load(&g);
    assert_eq!(loaded.persisted.rev, 2);
    assert!(loaded.rejected.is_empty(), "{:?}", loaded.rejected);
}

fn rev_of(path: &Path) -> u64 {
    decode(&fs::read(path).unwrap()).unwrap().rev
}

#[test]
fn a_boot_on_save_tmp_finishes_its_save() {
    // #32 review (major): after a boot on save.tmp it stayed the only
    // copy of the newest state, and the next save truncated it in place.
    // Recovery finishes the interrupted save before the engine runs.
    let (_d, s) = store();
    let g = test_site();
    s.save(&sample(5)).unwrap();
    fs::write(s.dir().join(TMP), encode(&sample(6)).unwrap()).unwrap();
    let loaded = s.load(&g);
    assert_eq!(loaded.source, Source::Interrupted);
    let done = s.recover(&loaded);
    assert_eq!(
        done,
        Recovery {
            finished: true,
            ..Recovery::default()
        }
    );
    assert!(!s.dir().join(TMP).exists());
    assert_eq!(rev_of(&s.dir().join(CURRENT)), 6);
    let gens = s.generations().unwrap();
    assert_eq!(gens.len(), 1);
    assert_eq!(rev_of(&gens[0].1), 5);
    // The next boot loads current.json and has nothing to recover.
    let again = s.load(&g);
    assert_eq!((again.source, again.persisted.rev), (Source::Current, 6));
    assert!(again.rejected.is_empty(), "{:?}", again.rejected);
    assert_eq!(s.recover(&again), Recovery::default());
}

#[test]
fn a_crash_during_recovery_is_finished_at_the_next_boot() {
    // Each step is a rename. A crash after the first leaves the old
    // current.json as a generation and save.tmp in place: the next boot
    // picks save.tmp again and finishes, adding no second generation.
    let (_d, s) = store();
    let g = test_site();
    s.save(&sample(5)).unwrap();
    fs::write(s.dir().join(TMP), encode(&sample(6)).unwrap()).unwrap();
    fs::rename(s.dir().join(CURRENT), s.dir().join("gen-0000000001.json")).unwrap();
    let loaded = s.load(&g);
    assert_eq!(
        (loaded.source, loaded.persisted.rev),
        (Source::Interrupted, 6)
    );
    assert!(s.recover(&loaded).finished);
    assert!(!s.dir().join(TMP).exists());
    assert_eq!(rev_of(&s.dir().join(CURRENT)), 6);
    let gens = s.generations().unwrap();
    assert_eq!(gens.len(), 1);
    assert_eq!(rev_of(&gens[0].1), 5);
}

#[test]
fn a_save_cut_off_after_an_interrupted_boot_keeps_the_loaded_state() {
    // Once recovered, the next save cut off while writing save.tmp
    // leaves current.json, the state the engine ran on, never the older
    // generation.
    let (_d, s) = store();
    let g = test_site();
    s.save(&sample(1)).unwrap();
    fs::write(s.dir().join(TMP), encode(&sample(2)).unwrap()).unwrap();
    fs::rename(s.dir().join(CURRENT), s.dir().join("gen-0000000001.json")).unwrap();
    let loaded = s.load(&g);
    s.recover(&loaded);
    let cut = encode(&sample(3)).unwrap();
    fs::write(s.dir().join(TMP), &cut[..cut.len() / 2]).unwrap();
    let after = s.load(&g);
    assert_eq!((after.source, after.persisted.rev), (Source::Current, 2));
}

#[test]
fn a_failed_recovery_step_is_reported() {
    // The engine still starts on the loaded state; the failure becomes
    // a SaveFailed alarm. Here save.tmp is gone before its rename.
    let (_d, s) = store();
    fs::write(s.dir().join(TMP), encode(&sample(4)).unwrap()).unwrap();
    let loaded = s.load(&test_site());
    assert_eq!(loaded.source, Source::Interrupted);
    fs::remove_file(s.dir().join(TMP)).unwrap();
    let done = s.recover(&loaded);
    assert!(!done.finished);
    assert_eq!(done.failed.len(), 1, "{:?}", done.failed);
    assert!(
        done.failed[0].starts_with("finishing the interrupted save failed"),
        "{:?}",
        done.failed
    );
}

/// `sample(rev)` told apart from another state at the same revision.
fn marked(rev: u64, mark: u64) -> Persisted {
    let mut p = sample(rev);
    p.saved_unix_ms = mark;
    p
}

/// current.json at 8 (damaged), generation 1 at 7.
fn damaged_current() -> (tempfile::TempDir, Store) {
    let (d, s) = store();
    s.save(&sample(7)).unwrap();
    s.save(&sample(8)).unwrap();
    corrupt(&s.dir().join(CURRENT));
    (d, s)
}

#[test]
fn a_damaged_current_json_weighs_save_tmp_against_the_newest_generation() {
    // #32 review (m1): with current.json damaged the chain never looked
    // at save.tmp. Now the higher revision of save.tmp and the newest
    // valid generation wins, a tie going to save.tmp.
    let g = test_site();
    let (_d, s) = damaged_current();
    fs::write(s.dir().join(TMP), encode(&sample(9)).unwrap()).unwrap();
    let loaded = s.load(&g);
    assert_eq!(
        (loaded.source, loaded.persisted.rev),
        (Source::Interrupted, 9)
    );
    assert_eq!(s.live_state().unwrap(), Some(Source::Interrupted));
    fs::write(s.dir().join(TMP), encode(&sample(7)).unwrap()).unwrap();
    assert_eq!(s.load(&g).source, Source::Interrupted);
    fs::write(s.dir().join(TMP), encode(&sample(6)).unwrap()).unwrap();
    let loaded = s.load(&g);
    assert_eq!(
        (loaded.source, loaded.persisted.rev),
        (Source::Generation(1), 7)
    );
    assert!(
        loaded.rejected.iter().any(|(path, why)| path.ends_with(TMP)
            && why == "revision 6 is not newer than generation 1's 7"),
        "{:?}",
        loaded.rejected
    );
    assert_eq!(s.live_state().unwrap(), Some(Source::Generation(1)));
}

#[test]
fn recovery_moves_a_damaged_current_json_aside_for_good() {
    let g = test_site();
    let (_d, s) = damaged_current();
    let damaged = fs::read(s.dir().join(CURRENT)).unwrap();
    let loaded = s.load(&g);
    assert_eq!(loaded.source, Source::Generation(1));
    let aside = s.dir().join("current.json.damaged-1");
    assert_eq!(
        s.recover(&loaded),
        Recovery {
            quarantined: Some(aside.clone()),
            ..Recovery::default()
        }
    );
    assert!(!s.dir().join(CURRENT).exists());
    assert_eq!(fs::read(&aside).unwrap(), damaged);
    // It is never read again, even if it held a valid state.
    fs::write(&aside, encode(&sample(99)).unwrap()).unwrap();
    assert_eq!(s.load(&g).persisted.rev, 7);
    // A later damaged current.json goes to the next free name.
    s.save(&sample(8)).unwrap();
    corrupt(&s.dir().join(CURRENT));
    let done = s.recover(&s.load(&g));
    assert_eq!(
        done.quarantined,
        Some(s.dir().join("current.json.damaged-2"))
    );
}

#[test]
fn recovery_moves_a_damaged_current_json_aside_then_finishes_the_save() {
    let g = test_site();
    let (_d, s) = damaged_current();
    fs::write(s.dir().join(TMP), encode(&sample(9)).unwrap()).unwrap();
    let loaded = s.load(&g);
    assert_eq!(loaded.source, Source::Interrupted);
    let done = s.recover(&loaded);
    assert_eq!(
        done,
        Recovery {
            quarantined: Some(s.dir().join("current.json.damaged-1")),
            finished: true,
            ..Recovery::default()
        }
    );
    assert_eq!(rev_of(&s.dir().join(CURRENT)), 9);
    assert!(!s.dir().join(TMP).exists());
    // The damaged file became no generation.
    let gens = s.generations().unwrap();
    assert_eq!(gens.len(), 1);
    assert_eq!(rev_of(&gens[0].1), 7);
}

#[test]
fn a_quarantine_that_fails_is_reported_and_the_save_still_finished() {
    // #32 P3: the engine still starts, and the interrupted save is
    // finished anyway (a damaged file that becomes a generation is
    // harmless: the chain skips generations that do not decode).
    let (_d, s) = damaged_current();
    fs::write(s.dir().join(TMP), encode(&sample(9)).unwrap()).unwrap();
    let loaded = s.load(&test_site());
    fs::remove_file(s.dir().join(CURRENT)).unwrap();
    let done = s.recover(&loaded);
    assert_eq!((done.quarantined.clone(), done.finished), (None, true));
    assert_eq!(done.failed.len(), 1, "{:?}", done.failed);
    assert!(
        done.failed[0].starts_with("the damaged current.json could not be moved aside"),
        "{:?}",
        done.failed
    );
    assert!(!s.dir().join(TMP).exists());
    assert_eq!(rev_of(&s.dir().join(CURRENT)), 9);
}

#[test]
fn a_revision_tie_prefers_save_tmp() {
    // #32 review (m2): save.tmp exists only for a save that was cut off,
    // newer by construction; at the same revision it is taken.
    let (_d, s) = store();
    s.save(&marked(10, 1)).unwrap();
    fs::write(s.dir().join(TMP), encode(&marked(10, 2)).unwrap()).unwrap();
    let loaded = s.load(&test_site());
    assert_eq!(loaded.source, Source::Interrupted);
    assert_eq!(loaded.persisted.saved_unix_ms, 2);
    assert_eq!(s.live_state().unwrap(), Some(Source::Interrupted));
}

#[test]
fn a_fallback_boot_then_edits_then_a_crash_keeps_the_newest_state() {
    // The review's scenario: current.json unusable at boot, so the core
    // restarts at an older generation's revision and edits bring it back
    // to the lost file's revision; a save cut off then must not lose to
    // that stale file. The stale file is moved aside at the fallback
    // boot, and a tie prefers save.tmp anyway.
    let g = test_site();
    let (_d, s) = store();
    s.save(&sample(7)).unwrap();
    s.save(&marked(10, 1)).unwrap();
    let stale = fs::read(s.dir().join(CURRENT)).unwrap();
    corrupt(&s.dir().join(CURRENT));
    let boot = s.load(&g);
    assert_eq!(
        (boot.source, boot.persisted.rev),
        (Source::Generation(1), 7)
    );
    assert!(s.recover(&boot).quarantined.is_some());
    // The engine edits from 7 back up to 10 and saves.
    s.save(&marked(10, 2)).unwrap();
    // The file that failed at boot turns readable again: it stays aside.
    fs::write(s.dir().join("current.json.damaged-1"), &stale).unwrap();
    // The next save, at the same revision (a save without a change), is
    // cut off after save.tmp was written.
    fs::write(s.dir().join(TMP), encode(&marked(10, 3)).unwrap()).unwrap();
    let loaded = s.load(&g);
    assert_eq!(loaded.source, Source::Interrupted);
    assert_eq!(loaded.persisted.saved_unix_ms, 3);
}

#[test]
fn live_state_names_only_what_the_load_chain_would_load() {
    // A damaged current.json alone is nothing the engine can load: the
    // seed may write its state (the damaged file is renamed, not lost).
    let (_d, s) = damaged_current();
    let generation = s.generations().unwrap()[0].1.clone();
    fs::remove_file(generation).unwrap();
    assert_eq!(s.live_state().unwrap(), None);
    // A current.json that cannot be read at all fails the seed closed.
    let (_d, s) = store();
    fs::create_dir(s.dir().join(CURRENT)).unwrap();
    assert!(s.live_state().is_err());
}

#[test]
fn a_boot_on_save_tmp_is_named_interrupted() {
    // The first save, cut off before its rename: save.tmp is the only state.
    let (_d, s) = store();
    fs::write(s.dir().join(TMP), encode(&sample(4)).unwrap()).unwrap();
    let loaded = s.load(&test_site());
    assert_eq!(loaded.source, Source::Interrupted);
    assert_eq!(loaded.persisted.rev, 4);
}

#[test]
fn a_newer_save_tmp_beside_current_json_wins() {
    // #32: a crash after save.tmp was written but before current.json
    // became a generation. save.tmp (revision N + 1) is the newest state.
    let (_d, s) = store();
    s.save(&sample(5)).unwrap();
    fs::write(s.dir().join(TMP), encode(&sample(6)).unwrap()).unwrap();
    let loaded = s.load(&test_site());
    assert_eq!(loaded.source, Source::Interrupted);
    assert_eq!(loaded.persisted.rev, 6);
    assert!(loaded.rejected.is_empty(), "{:?}", loaded.rejected);
}

#[test]
fn an_older_or_cut_off_save_tmp_is_skipped_and_named() {
    // Older by revision (a leftover baseline of an older engine) or cut
    // off while writing: current.json stands, and the skipped save.tmp is
    // reported.
    let (_d, s) = store();
    s.save(&sample(5)).unwrap();
    let cut = encode(&sample(6)).unwrap();
    for (tmp, why) in [
        (
            encode(&sample(4)).unwrap(),
            "revision 4 is not newer than current.json's 5",
        ),
        (cut[..cut.len() / 2].to_vec(), "not a state file"),
    ] {
        fs::write(s.dir().join(TMP), &tmp).unwrap();
        let loaded = s.load(&test_site());
        assert_eq!(loaded.source, Source::Current, "{why}");
        assert_eq!(loaded.persisted.rev, 5, "{why}");
        assert_eq!(loaded.rejected.len(), 1, "{why}: {:?}", loaded.rejected);
        assert!(loaded.rejected[0].0.ends_with(TMP));
        assert!(
            loaded.rejected[0].1.starts_with(why),
            "{:?}",
            loaded.rejected
        );
    }
}

#[test]
fn live_state_names_the_file_the_engine_would_load() {
    // Beside current.json, save.tmp is the live state only when it is a
    // complete state newer by revision, as in the load chain.
    let (_d, s) = store();
    s.save(&sample(5)).unwrap();
    fs::write(s.dir().join(TMP), encode(&sample(6)).unwrap()).unwrap();
    assert_eq!(s.live_state().unwrap(), Some(Source::Interrupted));
    fs::write(s.dir().join(TMP), encode(&sample(4)).unwrap()).unwrap();
    assert_eq!(s.live_state().unwrap(), Some(Source::Current));
    fs::write(s.dir().join(TMP), b"cut off").unwrap();
    assert_eq!(s.live_state().unwrap(), Some(Source::Current));
}

#[test]
fn an_older_save_tmp_beside_current_json_is_not_used() {
    let (_d, s) = store();
    s.save(&sample(2)).unwrap();
    fs::write(s.dir().join(TMP), encode(&sample(1)).unwrap()).unwrap();
    let loaded = s.load(&test_site());
    assert_eq!(loaded.source, Source::Current);
    assert_eq!(loaded.persisted.rev, 2);
}

#[test]
fn a_state_dir_that_cannot_be_listed_is_reported() {
    // #32 m3: the load chain stays tolerant, but a failed listing of the
    // generations is reported, never silently read as none.
    let (_d, s) = store();
    fs::remove_dir(s.dir()).unwrap();
    fs::write(s.dir(), b"not a directory").unwrap();
    let loaded = s.load(&test_site());
    assert_eq!(loaded.source, Source::Defaults);
    assert!(
        loaded
            .rejected
            .iter()
            .any(|(path, why)| path == s.dir()
                && why.starts_with("the generations cannot be listed")),
        "{:?}",
        loaded.rejected
    );
}

#[test]
fn baseline_is_the_last_resort_before_defaults() {
    let (_d, s) = store();
    s.save_baseline(&sample(9)).unwrap();
    fs::write(s.dir().join(CURRENT), b"{}").unwrap();
    let loaded = s.load(&test_site());
    assert_eq!(loaded.source, Source::Baseline);
    assert_eq!(loaded.persisted.rev, 9);
    assert_eq!(loaded.rejected.len(), 1);
}

#[test]
fn an_unreadable_file_is_rejected_not_skipped_as_missing() {
    let (_d, s) = store();
    s.save_baseline(&sample(4)).unwrap();
    // A directory where current.json belongs: reading it fails, not NotFound.
    fs::create_dir(s.dir().join(CURRENT)).unwrap();
    let loaded = s.load(&test_site());
    // The baseline's state (its revision is the load's to set: #32
    // MAJOR-3).
    assert_eq!(loaded.source, Source::Baseline);
    assert_eq!(loaded.persisted.saved_unix_ms, sample(4).saved_unix_ms);
    assert_eq!(loaded.rejected.len(), 1);
    assert!(loaded.rejected[0].0.ends_with(CURRENT));
}

#[test]
fn a_boot_past_an_unreadable_current_json_continues_its_revision_above_it() {
    // #32 MAJOR-3: current.json may hold any revision up to where the
    // session left off, and recovery never moves it. So whatever loads
    // instead (a generation, the baseline, the defaults) continues
    // 1 000 000 above the highest revision the state loaded or any name
    // in the directory shows (F3 round 4, finding 2: the marker names
    // current.json's 8), with an alarm: the session's saves then
    // outrank the file once it can be read again.
    let g = test_site();
    let (_d, s) = store();
    s.save(&sample(7)).unwrap();
    s.save(&sample(8)).unwrap();
    fs::remove_file(s.dir().join(CURRENT)).unwrap();
    fs::create_dir(s.dir().join(CURRENT)).unwrap();
    let loaded = s.load(&g);
    assert_eq!(
        (loaded.source, loaded.persisted.rev),
        (Source::Generation(1), 1_000_008)
    );
    let note = "current.json cannot be read, so the revision continues at 1000008";
    assert!(
        loaded.alarms.iter().any(|a| a.starts_with(note)),
        "{:?}",
        loaded.alarms
    );
    let generation = s.generations().unwrap()[0].1.clone();
    fs::remove_file(generation).unwrap();
    s.save_baseline(&sample(4)).unwrap();
    let loaded = s.load(&g);
    assert_eq!(
        (loaded.source, loaded.persisted.rev),
        (Source::Baseline, 1_000_008)
    );
    fs::remove_file(s.dir().join(BASELINE)).unwrap();
    let loaded = s.load(&g);
    assert_eq!(
        (loaded.source, loaded.persisted.rev),
        (Source::Defaults, 1_000_008)
    );
    // A readable current.json, damaged or not, moves nothing.
    fs::remove_dir(s.dir().join(CURRENT)).unwrap();
    fs::write(s.dir().join(CURRENT), b"damaged").unwrap();
    assert_eq!(s.load(&g).persisted.rev, 0);
}

/// The state directory's names, sorted.
fn names(s: &Store) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(s.dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn every_commit_writes_the_revisions_into_the_names() {
    // F3 round 4, finding 2: a floor only file contents carry is
    // unknowable exactly when they cannot be read. A generation's name
    // carries the revision it holds and the marker's name the one
    // current.json holds, so a listing shows them even then.
    let g = test_site();
    let (_d, s) = store();
    for rev in [3, 4, 5] {
        s.save(&sample(rev)).unwrap();
    }
    assert_eq!(
        names(&s),
        [
            "current.json",
            "current.json.rev-5",
            "gen-0000000001-r3.json",
            "gen-0000000002-r4.json"
        ]
    );
    // A boot's recovery that finishes save.tmp commits the same way.
    fs::write(s.dir().join(TMP), encode(&sample(6)).unwrap()).unwrap();
    let loaded = s.load(&g);
    assert!(s.recover(&loaded).finished);
    assert_eq!(
        names(&s),
        [
            "current.json",
            "current.json.rev-6",
            "gen-0000000001-r3.json",
            "gen-0000000002-r4.json",
            "gen-0000000003-r5.json"
        ]
    );
    // The marker shows what current.json holds, even below an older
    // revision (an import starts a new count).
    s.save(&sample(0)).unwrap();
    assert!(s.dir().join("current.json.rev-0").exists());
    assert!(s.dir().join("gen-0000000004-r6.json").exists());
    assert_eq!(
        (s.load(&g).source, s.generations().unwrap().len()),
        (Source::Current, 4)
    );
}

#[test]
fn the_jump_alarm_names_every_file_passed_over_and_the_floor() {
    // #32 F3-r4 2: one alarm names each live file the boot could not
    // read and the highest revision the names show.
    let g = test_site();
    let (_d, s) = store();
    for rev in 7..=9 {
        s.save(&sample(rev)).unwrap();
    }
    let newer = s.generations().unwrap()[1].1.clone();
    fs::remove_file(s.dir().join(CURRENT)).unwrap();
    fs::create_dir(s.dir().join(CURRENT)).unwrap();
    let loaded = s.load(&g);
    assert_eq!(
        (loaded.source, loaded.persisted.rev),
        (Source::Generation(2), 1_000_009)
    );
    assert_eq!(
        loaded.alarms.last().unwrap(),
        "current.json cannot be read, so the revision continues at 1000009, \
         above anything it can hold (the names show revision 9 at most)"
    );
    fs::remove_file(&newer).unwrap();
    fs::create_dir(&newer).unwrap();
    let loaded = s.load(&g);
    assert_eq!(
        (loaded.source, loaded.persisted.rev),
        (Source::Generation(1), 1_000_009)
    );
    assert_eq!(
        loaded.alarms.last().unwrap(),
        "current.json and gen-0000000002-r8.json cannot be read, so the revision \
         continues at 1000009, above anything they can hold (the names show revision 9 \
         at most)"
    );
    // No name shows a revision: the state loaded's own is the base.
    let (_d, s) = store();
    fs::write(
        s.dir().join("gen-0000000001.json"),
        encode(&sample(4)).unwrap(),
    )
    .unwrap();
    fs::create_dir(s.dir().join(CURRENT)).unwrap();
    let loaded = s.load(&g);
    assert_eq!(
        loaded.alarms.last().unwrap(),
        "current.json cannot be read, so the revision continues at 1000004, \
         above anything it can hold"
    );
}

#[test]
fn a_state_directory_with_an_older_engines_names_loads_as_before() {
    // F3 round 4, finding 2: the state directory on the PC has names
    // without a revision (gen-<seq>.json, no marker). They load as
    // before, and past an unreadable current.json the revision jumps
    // from the state loaded, as no name shows one. The first save names
    // the rotated current.json the old way (no marker tells its
    // revision) and starts the marker; from then on the names carry
    // revisions.
    let g = test_site();
    let (_d, s) = store();
    let put = |name: &str, rev: u64| {
        fs::write(s.dir().join(name), encode(&sample(rev)).unwrap()).unwrap();
    };
    put("gen-0000000001.json", 3);
    put("gen-0000000002.json", 4);
    put(CURRENT, 5);
    let loaded = s.load(&g);
    assert_eq!((loaded.source, loaded.persisted.rev), (Source::Current, 5));
    assert!(
        loaded.alarms.is_empty() && loaded.rejected.is_empty(),
        "{loaded:?}"
    );
    let seqs: Vec<u64> = s.generations().unwrap().iter().map(|g| g.0).collect();
    assert_eq!(seqs, [1, 2]);
    assert_eq!(s.live_state().unwrap(), Some(Source::Current));
    // Past an unreadable current.json: the generation's own revision.
    fs::remove_file(s.dir().join(CURRENT)).unwrap();
    fs::create_dir(s.dir().join(CURRENT)).unwrap();
    let loaded = s.load(&g);
    assert_eq!(
        (loaded.source, loaded.persisted.rev),
        (Source::Generation(2), 1_000_004)
    );
    fs::remove_dir(s.dir().join(CURRENT)).unwrap();
    put(CURRENT, 5);
    s.save(&sample(6)).unwrap();
    s.save(&sample(7)).unwrap();
    assert_eq!(
        names(&s),
        [
            "current.json",
            "current.json.rev-7",
            "gen-0000000001.json",
            "gen-0000000002.json",
            "gen-0000000003.json",
            "gen-0000000004-r6.json"
        ]
    );
    let loaded = s.load(&g);
    assert_eq!((loaded.source, loaded.persisted.rev), (Source::Current, 7));
}

#[test]
fn an_older_save_tmp_raises_an_alarm_naming_it() {
    // #32 MAJOR-3: a valid save.tmp older than the state it competes
    // with is only legitimate as a leftover. It is not loaded, and not
    // silently: an alarm names it (the next save moves it aside).
    let g = test_site();
    let (_d, s) = store();
    s.save(&sample(5)).unwrap();
    s.save(&sample(6)).unwrap();
    fs::write(s.dir().join(TMP), encode(&sample(4)).unwrap()).unwrap();
    let loaded = s.load(&g);
    assert_eq!((loaded.source, loaded.persisted.rev), (Source::Current, 6));
    assert_eq!(
        loaded.alarms,
        ["save.tmp (revision 4) is older than current.json's 6 and is not loaded"]
    );
    // Against the newest valid generation when current.json is damaged.
    corrupt(&s.dir().join(CURRENT));
    let loaded = s.load(&g);
    assert_eq!(
        (loaded.source, loaded.persisted.rev),
        (Source::Generation(1), 5)
    );
    assert_eq!(
        loaded.alarms,
        ["save.tmp (revision 4) is older than generation 1's 5 and is not loaded"]
    );
}

#[test]
fn an_orphan_above_the_state_loaded_raises_an_alarm() {
    // F3 round 4, finding 1: an orphan is never loaded, but one that
    // holds a revision above the state that loaded is named in an
    // alarm, never passed over silently. One at or below it, one that
    // does not decode, and a name the store never writes are left
    // alone.
    let g = test_site();
    let (_d, s) = store();
    s.save(&sample(5)).unwrap();
    let aside = |name: &str, bytes: &[u8]| fs::write(s.dir().join(name), bytes).unwrap();
    aside("save.tmp.orphan-1", &encode(&sample(5)).unwrap());
    aside("save.tmp.orphan-2", b"cut off");
    aside("save.tmp.orphan-x", &encode(&sample(9)).unwrap());
    let loaded = s.load(&g);
    assert!(loaded.alarms.is_empty(), "{:?}", loaded.alarms);
    aside("save.tmp.orphan-3", &encode(&sample(6)).unwrap());
    let loaded = s.load(&g);
    assert_eq!((loaded.source, loaded.persisted.rev), (Source::Current, 5));
    assert_eq!(
        loaded.alarms,
        [
            "save.tmp.orphan-3 (revision 6) is above the state loaded (revision 5): \
          a save.tmp moved aside, kept but never loaded"
        ]
    );
    assert!(loaded.rejected.is_empty(), "{:?}", loaded.rejected);
}

#[test]
fn a_read_is_tried_again_only_for_an_error_that_may_pass() {
    use io::ErrorKind::{
        Interrupted, IsADirectory, NotADirectory, Other, PermissionDenied, TimedOut, WouldBlock,
    };
    for kind in [Interrupted, WouldBlock, TimedOut] {
        assert!(transient(&io::Error::from(kind)), "{kind:?}");
    }
    for kind in [IsADirectory, NotADirectory, PermissionDenied, Other] {
        assert!(!transient(&io::Error::from(kind)), "{kind:?}");
    }
    // Windows' sharing and lock violations; elsewhere these codes are
    // other errors (EPIPE, EDOM on Linux).
    for code in [SHARING_VIOLATION, LOCK_VIOLATION] {
        let e = io::Error::from_raw_os_error(code);
        assert_eq!(transient(&e), cfg!(windows), "{code}: {e}");
    }
}

#[test]
fn nothing_loadable_gives_muted_defaults() {
    let (_d, s) = store();
    let g = test_site();
    let loaded = s.load(&g);
    assert_eq!(loaded.source, Source::Defaults);
    assert!(loaded.rejected.is_empty());
    assert_eq!(loaded.persisted.rev, 0);
    assert_eq!(loaded.persisted.topology_hash, g.hash);
    for n in &g.mixes {
        assert!(loaded.persisted.state.mixes[&n.id].out.muted, "{}", n.id);
    }
}

#[test]
fn newer_fields_are_ignored_and_missing_default() {
    let (_d, s) = store();
    let payload =
        r#"{"rev":7,"state":{"inputs":{"mic1":{"trim_db":-2.0,"colour":"red"}}},"future":{"x":1}}"#;
    let file = format!(
        r#"{{"format":"iemmixer-state","schema":9,"sha256":"{}","payload":{payload},"later":true}}"#,
        digest(payload)
    );
    fs::write(s.dir().join(CURRENT), file).unwrap();
    let loaded = s.load(&test_site());
    assert_eq!(loaded.source, Source::Current);
    assert_eq!(loaded.persisted.rev, 7);
    assert!(loaded.persisted.counters.is_empty());
    let mic1 = loaded.persisted.state.inputs[&InputId::new("mic1")];
    assert_eq!(mic1.trim_db, -2.0);
    assert!(mic1.processing);
    assert_eq!(loaded.persisted.state.mixes.len(), 11);
}

#[test]
fn unknown_ids_are_dropped_on_load() {
    let (_d, s) = store();
    let mut p = sample(2);
    p.state
        .inputs
        .insert(InputId::new("ghost"), InputState::default());
    s.save(&p).unwrap();
    let loaded = s.load(&test_site());
    assert_eq!(loaded.dropped, vec!["input ghost"]);
    assert!(
        !loaded
            .persisted
            .state
            .inputs
            .contains_key(&InputId::new("ghost"))
    );
    assert_eq!(loaded.persisted.state.inputs.len(), 24);
}
