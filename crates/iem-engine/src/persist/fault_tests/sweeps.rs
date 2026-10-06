//! Every single failure (#32 P9, minor-8, F3-r4 7): save, recovery, the
//! move aside of a damaged current.json and of a save.tmp the boot could not
//! read, and the boot's own reads, each failed at every step once and from
//! there on (a crash), then a boot and the session's next save.

use super::*;

/// A full directory: generations 1 to 20 at their revisions and
/// current.json at 21, so a commit also prunes.
fn filled(s: &Store) {
    for rev in 1..=20 {
        let name = format!("gen-{rev:010}.json");
        fs::write(s.dir().join(name), encode(&sample(rev)).unwrap()).unwrap();
    }
    fs::write(s.dir().join(CURRENT), encode(&sample(21)).unwrap()).unwrap();
}

/// The newest state a boot must not lose: the higher revision of a valid
/// current.json and a valid save.tmp (never save.new, never a generation).
fn newest(dir: &Path) -> u64 {
    [CURRENT, TMP]
        .iter()
        .filter_map(|name| fs::read(dir.join(name)).ok())
        .filter_map(|bytes| decode(&bytes).ok())
        .map(|p| p.rev)
        .max()
        .unwrap()
}

/// The `save.tmp`s moved aside in `dir` that hold a state above `rev`, by
/// name.
fn orphans_over(dir: &Path, rev: u64) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| {
            let e = e.unwrap();
            let name = e.file_name().to_string_lossy().into_owned();
            let held = decode(&fs::read(e.path()).ok()?).ok()?;
            (name.starts_with(ORPHAN) && held.rev > rev).then_some(name)
        })
        .collect();
    names.sort();
    names
}

/// Runs `op` after `setup` with each single failure it can meet: once (an
/// error, the engine runs on) and from that step on (a crash: the next
/// process boots and recovers). After it, a boot loads the newest
/// committed or pending state, with an alarm only for each orphan above it
/// (#32 F3-r4 7: a move aside cut off right after it); the engine then saves its
/// next edit (the boot's revision + 1), which is the newest state on disk
/// and what the next boot loads (#32 minor-8).
fn every_failure<T>(what: &str, setup: impl Fn(&Store, &Faulty) -> T, op: impl Fn(&Store, &T)) {
    let g = test_site();
    let (_d, faulty, s) = faulty_store();
    let prepared = setup(&s, &faulty);
    faulty.count();
    op(&s, &prepared);
    let steps = faulty.steps();
    assert!(steps > 3, "{what}: {steps} steps");
    for mode in [Mode::Once, Mode::From] {
        for at in 0..steps {
            let (_d, faulty, s) = faulty_store();
            let prepared = setup(&s, &faulty);
            faulty.arm(at, mode);
            op(&s, &prepared);
            faulty.count();
            let want = newest(s.dir());
            // A crash ends the process: the boot is the next one's.
            let s = match mode {
                Mode::Once => s,
                Mode::From => reopen(s.dir(), &faulty),
            };
            let boot = s.load(&g);
            assert_eq!(
                boot.persisted.rev, want,
                "{what}, {mode:?} at step {at}: the first boot"
            );
            // Never an older state silently (#32 F3-r4 7): an orphan above
            // the boot's state is named in an alarm, and nothing else
            // raises one.
            let above = orphans_over(s.dir(), boot.persisted.rev);
            assert_eq!(
                boot.alarms.len(),
                above.len(),
                "{what}, {mode:?} at {at}: {boot:?}"
            );
            for name in &above {
                assert!(
                    boot.alarms
                        .iter()
                        .any(|a| a.starts_with(&format!("{name} ("))),
                    "{what}, {mode:?} at {at}: {name} unnamed in {:?}",
                    boot.alarms
                );
            }
            if mode == Mode::From {
                let done = s.recover(&boot);
                assert!(done.failed.is_empty(), "{what}, {mode:?} at {at}: {done:?}");
            }
            let next = boot.persisted.rev + 1;
            s.save(&session(next)).unwrap();
            assert_eq!(
                newest(s.dir()),
                next,
                "{what}, {mode:?} at step {at}: the newest state after the save"
            );
            let again = reopen(s.dir(), &faulty).load(&g);
            assert_eq!(
                (
                    again.source,
                    again.persisted.rev,
                    again.persisted.saved_unix_ms
                ),
                (Source::Current, next, SESSION),
                "{what}, {mode:?} at step {at}: after the next save"
            );
            assert!(
                again.alarms.is_empty(),
                "{what}, {mode:?} at {at}: {again:?}"
            );
            assert!(!s.dir().join(TMP).exists() && !s.dir().join(NEW).exists());
        }
    }
}

#[test]
fn a_save_survives_every_single_failure() {
    every_failure(
        "save",
        |s, _| filled(s),
        |s, _| {
            let _ = s.save(&sample(22));
        },
    );
}

#[test]
fn a_recovery_survives_every_single_failure() {
    every_failure(
        "recover",
        |s, _| {
            filled(s);
            fs::write(s.dir().join(TMP), encode(&sample(22)).unwrap()).unwrap();
            let loaded = s.load(&test_site());
            assert_eq!(loaded.source, Source::Interrupted);
            loaded
        },
        |s, loaded| {
            s.recover(loaded);
        },
    );
}

#[test]
fn a_recovery_with_a_move_aside_survives_every_single_failure() {
    every_failure(
        "quarantine",
        |s, _| {
            filled(s);
            fs::write(s.dir().join(CURRENT), b"damaged").unwrap();
            fs::write(s.dir().join(TMP), encode(&sample(22)).unwrap()).unwrap();
            let loaded = s.load(&test_site());
            assert_eq!(
                (loaded.source, loaded.current_json),
                (Source::Interrupted, FileState::Damaged)
            );
            loaded
        },
        |s, loaded| {
            s.recover(loaded);
        },
    );
}

#[test]
fn a_save_that_moves_a_save_tmp_aside_survives_every_single_failure() {
    // F3 round 4, finding 7: no sweep failed the move aside. The boot could
    // not read save.tmp (another process held it a moment), so the
    // session's save moves it aside: every failure there (asking whether
    // save.tmp and the orphan's name exist, the rename, the directory sync)
    // and after it, each followed by a crash, a boot and a save, leaves no
    // silent rollback: the boot loads the newest committed or pending
    // state, or names in an alarm the orphan above it.
    every_failure(
        "orphan",
        |s, faulty| {
            filled(s);
            let tmp = s.dir().join(TMP);
            fs::write(&tmp, encode(&sample(22)).unwrap()).unwrap();
            faulty.set_unreadable(&tmp, true);
            let boot = s.load(&test_site());
            assert_eq!(
                (boot.source, boot.persisted.rev, boot.save_tmp),
                (Source::Current, 21, FileState::Unreadable)
            );
            s.recover(&boot);
            faulty.set_unreadable(&tmp, false);
        },
        |s, _| {
            let _ = s.save(&session(22));
        },
    );
}

#[test]
fn a_boot_whose_reads_fail_never_rolls_back_silently() {
    // A read that fails once is tried again; reads that keep failing may
    // load an older state, but never without an alarm (or a fallback
    // source, which the engine alarms on), and leave the files as they are.
    let g = test_site();
    let pending = encode(&sample(22)).unwrap();
    let setup = |s: &Store| {
        filled(s);
        fs::write(s.dir().join(TMP), &pending).unwrap();
    };
    let (_d, faulty, s) = faulty_store();
    setup(&s);
    faulty.count();
    assert_eq!(s.load(&g).persisted.rev, 22);
    let steps = faulty.steps();
    for mode in [Mode::Once, Mode::From] {
        for at in 0..steps {
            let (_d, faulty, s) = faulty_store();
            setup(&s);
            faulty.arm(at, mode);
            let boot = s.load(&g);
            let alarmed = !boot.alarms.is_empty()
                || !matches!(boot.source, Source::Current | Source::Interrupted);
            assert!(
                boot.persisted.rev == 22 || alarmed,
                "{mode:?} at step {at}: {boot:?}"
            );
            if mode == Mode::Once {
                assert_eq!(boot.persisted.rev, 22, "{mode:?} at step {at}");
            }
            faulty.count();
            assert_eq!(s.load(&g).persisted.rev, 22, "{mode:?} at step {at}");
            // #32 minor-8: the engine then runs on the boot's state and
            // saves its next edit. The next boot loads that save, and the
            // newest state before the boot is still in the directory (a
            // save.tmp the boot could not read is moved aside, never
            // replaced).
            s.recover(&boot);
            let next = boot.persisted.rev + 1;
            s.save(&session(next)).unwrap();
            let again = reopen(s.dir(), &faulty).load(&g);
            assert_eq!(
                (
                    again.source,
                    again.persisted.rev,
                    again.persisted.saved_unix_ms
                ),
                (Source::Current, next, SESSION),
                "{mode:?} at step {at}: after the session's save"
            );
            assert!(again.alarms.is_empty(), "{mode:?} at {at}: {again:?}");
            assert!(
                kept(s.dir(), &pending),
                "{mode:?} at step {at}: save.tmp lost"
            );
        }
    }
}
