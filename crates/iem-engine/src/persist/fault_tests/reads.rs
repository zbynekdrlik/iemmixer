//! Reads under injected failures (#32 P2, minor-5, F3-r4): a file that
//! cannot be read is tried again only for an error that may pass, within
//! the load's bound, and named in an alarm; what leaves the live state in
//! doubt is a doubt; the seed's strict pick fails where the boot goes on.

use super::*;

/// A read of a file that keeps failing: tried `READ_TRIES` times.
const TRIES: usize = 5;

#[test]
fn an_unreadable_current_json_is_never_moved_or_rotated() {
    // #32 P2: an I/O error (a lock, no access, a failing disk) is no damage.
    // The chain raises an alarm naming the file and loads the best valid
    // state; recovery never moves the file aside, renames or rotates it.
    let g = test_site();
    let (_d, faulty, s) = faulty_store();
    s.save(&sample(7)).unwrap();
    s.save(&sample(8)).unwrap();
    fs::write(s.dir().join(TMP), encode(&sample(9)).unwrap()).unwrap();
    let current = s.dir().join(CURRENT);
    let bytes = fs::read(&current).unwrap();
    faulty.set_unreadable(&current, true);
    let loaded = s.load(&g);
    // save.tmp's state (its revision is the load's to set: #32 MAJOR-3).
    assert_eq!(
        (loaded.source, loaded.persisted.saved_unix_ms),
        (Source::Interrupted, sample(9).saved_unix_ms)
    );
    assert_eq!(loaded.current_json, FileState::Unreadable);
    assert_eq!(faulty.pauses(), TRIES - 1);
    assert!(
        loaded
            .alarms
            .iter()
            .any(|a| a.starts_with("current.json cannot be read")),
        "{:?}",
        loaded.alarms
    );
    let done = s.recover(&loaded);
    assert_eq!(
        (done.quarantined.clone(), done.finished),
        (None, false),
        "{done:?}"
    );
    assert!(done.failed.is_empty(), "{done:?}");
    assert_eq!(done.warnings.len(), 1, "{done:?}");
    assert_eq!(fs::read(&current).unwrap(), bytes);
    assert_eq!(rev_of(&s.dir().join(TMP)), 9);
    // Once readable again, the runtime's next save rotates it into the
    // generations like any current.json: kept in the history.
    faulty.set_unreadable(&current, false);
    s.save(&sample(10)).unwrap();
    let gens = s.generations().unwrap();
    assert_eq!(gens.len(), 2);
    assert_eq!(fs::read(&gens[1].1).unwrap(), bytes);
    assert_eq!(s.load(&g).persisted.rev, 10);
}

#[test]
fn a_read_that_fails_briefly_is_tried_again() {
    let g = test_site();
    let (_d, faulty, s) = faulty_store();
    s.save(&sample(5)).unwrap();
    let current = s.dir().join(CURRENT);
    faulty.flaky(&current, TRIES - 1);
    let loaded = s.load(&g);
    assert_eq!((loaded.source, loaded.persisted.rev), (Source::Current, 5));
    assert!(
        loaded.alarms.is_empty() && loaded.rejected.is_empty(),
        "{loaded:?}"
    );
    assert_eq!(faulty.pauses(), TRIES - 1);
    // The seed's strict pick tries again too; a file that stays unreadable
    // fails it.
    faulty.flaky(&current, TRIES - 1);
    assert_eq!(s.live_state().unwrap(), Some(Source::Current));
    faulty.set_unreadable(&current, true);
    assert!(s.live_state().is_err());
}

#[test]
fn a_read_that_keeps_failing_is_tried_a_bounded_number_of_times() {
    // CI run 37466176804 (caught only by a timeout): with read_tried's
    // guard mutated (`||`, or always true) a read failing with an error
    // that may pass was tried forever, and the tests past a locked
    // current.json hung until nextest ended them. Here a read beyond the
    // bound panics at once, so such a loop fails this test in
    // milliseconds; it runs first under the mutants profile (priority).
    let (_d, faulty, s) = faulty_store();
    s.save(&sample(5)).unwrap();
    let current = s.dir().join(CURRENT);
    faulty.set_unreadable(&current, true);
    faulty.limit_reads(&current, TRIES);
    let loaded = s.load(&test_site());
    assert_eq!(loaded.current_json, FileState::Unreadable);
    assert_eq!(
        (faulty.limited_reads(), faulty.pauses()),
        (TRIES, TRIES - 1)
    );
    // The seed's strict pick has the same bound.
    faulty.limit_reads(&current, TRIES);
    assert!(s.live_state().is_err());
    assert_eq!(faulty.limited_reads(), TRIES);
}

#[test]
fn only_what_leaves_the_live_state_in_doubt_is_a_doubt() {
    // #32 F3-r4 6: an import refuses on `Loaded::doubts` only. An older
    // save.tmp (its save moves it aside) and an orphan above the state
    // loaded are alarms, no doubt.
    let g = test_site();
    let (_d, _f, s) = faulty_store();
    s.save(&sample(5)).unwrap();
    fs::write(s.dir().join(TMP), encode(&sample(4)).unwrap()).unwrap();
    fs::write(
        s.dir().join("save.tmp.orphan-1"),
        encode(&sample(9)).unwrap(),
    )
    .unwrap();
    let loaded = s.load(&g);
    assert_eq!(loaded.alarms.len(), 2, "{:?}", loaded.alarms);
    assert!(loaded.doubts.is_empty(), "{:?}", loaded.doubts);
    // A file that cannot be read, and the revision continued above it.
    let (_d, faulty, s) = faulty_store();
    s.save(&sample(7)).unwrap();
    s.save(&sample(8)).unwrap();
    faulty.set_unreadable(&s.dir().join(CURRENT), true);
    let loaded = s.load(&g);
    assert_eq!(loaded.doubts.len(), 2, "{:?}", loaded.doubts);
    assert_eq!(loaded.doubts, loaded.alarms);
    // A save.tmp loaded without a comparison.
    let (_d, faulty, s) = faulty_store();
    s.save(&sample(7)).unwrap();
    fs::write(s.dir().join(CURRENT), b"damaged").unwrap();
    fs::write(s.dir().join(TMP), encode(&sample(9)).unwrap()).unwrap();
    faulty.fail_next("list");
    let loaded = s.load(&g);
    assert_eq!(loaded.source, Source::Interrupted);
    assert_eq!(loaded.doubts.len(), 1, "{:?}", loaded.doubts);
    assert_eq!(loaded.doubts, loaded.alarms);
}

#[test]
fn the_seed_fails_when_the_generations_cannot_be_listed() {
    // CI run 37466176804 (a surviving mutant): with current.json missing
    // the live state may be a generation, so a listing that fails is the
    // seed's error (its strict pick), never "no state" (#32 m3: the seed
    // would write over generations it could not see), while the boot's
    // tolerant pick goes on with an alarm.
    let (_d, faulty, s) = faulty_store();
    s.save(&sample(7)).unwrap();
    s.save(&sample(8)).unwrap();
    fs::remove_file(s.dir().join(CURRENT)).unwrap();
    faulty.fail_next("list");
    assert!(s.live_state().is_err());
    faulty.fail_next("list");
    assert!(s.live_file().is_err());
    assert_eq!(s.live_state().unwrap(), Some(Source::Generation(1)));
    faulty.fail_next("list");
    let loaded = s.load(&test_site());
    assert_eq!(loaded.source, Source::Defaults);
    assert!(
        loaded
            .rejected
            .iter()
            .any(|(_, why)| why.starts_with("the generations cannot be listed")),
        "{:?}",
        loaded.rejected
    );
}

#[test]
fn only_a_read_error_that_may_pass_is_tried_again() {
    // #32 minor-5: a directory where current.json belongs (IsADirectory on
    // Linux, access denied on Windows) does not turn readable by waiting:
    // it is Unreadable at once, with no pause.
    let (_d, faulty, s) = faulty_store();
    s.save_baseline(&sample(4)).unwrap();
    fs::create_dir(s.dir().join(CURRENT)).unwrap();
    let loaded = s.load(&test_site());
    assert_eq!(
        (loaded.source, loaded.current_json),
        (Source::Baseline, FileState::Unreadable)
    );
    assert_eq!(faulty.pauses(), 0);
}

#[test]
fn a_boot_pauses_between_read_tries_two_seconds_at_most() {
    // #32 minor-5: each file that stays locked took its four pauses (0.8 s
    // each), so a few of them held the engine past the guard's READY_S
    // (10 s) before it listened. The pauses of one load are 10 in all
    // (2 s); a file read once they are spent still gets its one try.
    let (_d, faulty, s) = faulty_store();
    for rev in 1..=3 {
        s.save(&sample(rev)).unwrap();
    }
    fs::write(s.dir().join(TMP), encode(&sample(4)).unwrap()).unwrap();
    let generation = s.generations().unwrap()[1].1.clone();
    for path in [s.dir().join(CURRENT), s.dir().join(TMP), generation] {
        faulty.set_locked(&path, true);
    }
    let loaded = s.load(&test_site());
    assert_eq!(faulty.pauses(), 10);
    assert_eq!(loaded.source, Source::Generation(1));
    let unreadable = loaded
        .alarms
        .iter()
        .filter(|a| a.contains(" cannot be read ("))
        .count();
    assert_eq!(unreadable, 3, "{:?}", loaded.alarms);
}

#[test]
fn an_unreadable_save_tmp_or_generation_is_named_in_an_alarm() {
    let g = test_site();
    let (_d, faulty, s) = faulty_store();
    s.save(&sample(5)).unwrap();
    fs::write(s.dir().join(TMP), encode(&sample(6)).unwrap()).unwrap();
    faulty.set_unreadable(&s.dir().join(TMP), true);
    let loaded = s.load(&g);
    assert_eq!((loaded.source, loaded.persisted.rev), (Source::Current, 5));
    assert_eq!(loaded.alarms.len(), 1, "{:?}", loaded.alarms);
    assert!(
        loaded.alarms[0].starts_with("save.tmp cannot be read"),
        "{:?}",
        loaded.alarms
    );
    // A generation passed over while looking for the newest valid one; it
    // may hold any revision up to what the names show (the marker's 9), so
    // the revision continues above that (F3 round 4, finding 2).
    let (_d, faulty, s) = faulty_store();
    for rev in 7..=9 {
        s.save(&sample(rev)).unwrap();
    }
    fs::remove_file(s.dir().join(CURRENT)).unwrap();
    let generation = s.generations().unwrap()[1].1.clone();
    faulty.set_unreadable(&generation, true);
    let loaded = s.load(&g);
    assert_eq!(
        (loaded.source, loaded.persisted.rev),
        (Source::Generation(1), 1_000_009)
    );
    assert_eq!(loaded.alarms.len(), 2, "{:?}", loaded.alarms);
    assert!(
        loaded.alarms[0].starts_with("gen-0000000002-r8.json cannot be read ("),
        "{:?}",
        loaded.alarms
    );
    assert!(
        loaded.alarms[1].starts_with(
            "gen-0000000002-r8.json cannot be read, so the revision continues at 1000009"
        ),
        "{:?}",
        loaded.alarms
    );
}

#[test]
fn save_tmp_that_could_not_be_compared_is_loaded_only_with_an_alarm() {
    // #32 P8: with current.json missing or damaged, save.tmp competes with
    // the newest valid generation; if the generations cannot be listed its
    // revision was compared with nothing.
    let (_d, faulty, s) = faulty_store();
    s.save(&sample(7)).unwrap();
    fs::write(s.dir().join(CURRENT), b"damaged").unwrap();
    fs::write(s.dir().join(TMP), encode(&sample(9)).unwrap()).unwrap();
    faulty.fail_next("list");
    let loaded = s.load(&test_site());
    assert_eq!(
        (loaded.source, loaded.persisted.rev),
        (Source::Interrupted, 9)
    );
    assert_eq!(loaded.alarms.len(), 1, "{:?}", loaded.alarms);
    assert!(
        loaded.alarms[0].starts_with("save.tmp is loaded without comparing"),
        "{:?}",
        loaded.alarms
    );
}
