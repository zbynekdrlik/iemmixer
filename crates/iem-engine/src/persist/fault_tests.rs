//! Persistence under injected file-system failures (#32 P9): a `Files` that
//! fails a chosen step (once, or from that step on, as a crash or a full
//! disk would), and the protocol's tests: the newest committed or pending
//! state survives every single failure, and an older state is never loaded
//! without an alarm.

use std::sync::{Arc, Mutex};

use super::files::{Entries, Files, OsFiles};
use super::tests::sample;
use super::*;
use crate::test_support::test_site;

/// How an armed fault fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    /// Only the chosen step fails.
    Once,
    /// The chosen step and every one after it fail (a crash, a full disk).
    From,
}

#[derive(Debug, Default)]
struct FaultState {
    /// Steps taken since the fault was armed.
    steps: usize,
    fault: Option<(usize, Mode)>,
    /// The next step of this kind fails, once.
    next: Option<&'static str>,
    /// Paths every read of which fails (an I/O error, not a missing file).
    unreadable: Vec<PathBuf>,
    /// Paths another process holds open without sharing (a Windows sharing
    /// violation): every read fails and so does every rename from or to
    /// them.
    locked: Vec<PathBuf>,
    /// Read failures still to come per path; then its reads succeed.
    flaky: Vec<(PathBuf, usize)>,
    /// The next rename onto this path fails, once.
    rename_to: Option<PathBuf>,
    /// Pauses between read tries.
    pauses: usize,
}

/// A failure this file system injects: a kind the chain retries, as it
/// would a sharing violation or a transient EIO.
fn injected(what: String) -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, what)
}

/// The real file system with injected failures. A failed write leaves half
/// of its bytes (a write cut off); any other failed step does nothing.
#[derive(Debug, Default)]
pub(super) struct Faulty {
    state: Mutex<FaultState>,
}

impl Faulty {
    /// Fails step `at` (0 = the next one) as `mode` says.
    pub(super) fn arm(&self, at: usize, mode: Mode) {
        let mut s = self.state.lock().unwrap();
        s.steps = 0;
        s.fault = Some((at, mode));
    }

    /// Counts steps from now without failing any.
    pub(super) fn count(&self) {
        let mut s = self.state.lock().unwrap();
        s.steps = 0;
        s.fault = None;
        s.next = None;
    }

    /// Steps taken since `arm` or `count`.
    pub(super) fn steps(&self) -> usize {
        self.state.lock().unwrap().steps
    }

    /// The next step of kind `what` ("write", "rename", "remove", …) fails.
    pub(super) fn fail_next(&self, what: &'static str) {
        self.state.lock().unwrap().next = Some(what);
    }

    /// Every read of `path` fails with an I/O error while `unreadable`.
    pub(super) fn set_unreadable(&self, path: &Path, unreadable: bool) {
        let mut s = self.state.lock().unwrap();
        s.unreadable.retain(|p| p != path);
        if unreadable {
            s.unreadable.push(path.to_path_buf());
        }
    }

    /// Another process holds `path` while `locked`: reads of it and renames
    /// from or to it fail.
    pub(super) fn set_locked(&self, path: &Path, locked: bool) {
        let mut s = self.state.lock().unwrap();
        s.locked.retain(|p| p != path);
        if locked {
            s.locked.push(path.to_path_buf());
        }
    }

    /// The next `times` reads of `path` fail, then they succeed.
    pub(super) fn flaky(&self, path: &Path, times: usize) {
        let mut s = self.state.lock().unwrap();
        s.flaky.push((path.to_path_buf(), times));
    }

    /// The next rename onto `path` fails, once (the rename that follows a
    /// move aside, say).
    pub(super) fn fail_rename_to(&self, path: &Path) {
        self.state.lock().unwrap().rename_to = Some(path.to_path_buf());
    }

    /// Pauses between read tries so far.
    pub(super) fn pauses(&self) -> usize {
        self.state.lock().unwrap().pauses
    }

    fn step(&self, what: &str, path: &Path) -> io::Result<()> {
        let mut s = self.state.lock().unwrap();
        let at = s.steps;
        s.steps += 1;
        let mut fails = match s.fault {
            Some((n, Mode::Once)) => at == n,
            Some((n, Mode::From)) => at >= n,
            None => false,
        };
        if s.next == Some(what) {
            s.next = None;
            fails = true;
        }
        if fails {
            return Err(injected(format!(
                "injected failure at step {at}: {what} {}",
                path.display()
            )));
        }
        Ok(())
    }
}

impl Files for Faulty {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.step("read", path)?;
        {
            let mut s = self.state.lock().unwrap();
            if s.unreadable.iter().chain(&s.locked).any(|p| p == path) {
                return Err(injected(format!("unreadable: {}", path.display())));
            }
            if let Some(f) = s.flaky.iter_mut().find(|(p, n)| p == path && *n > 0) {
                f.1 -= 1;
                return Err(injected(format!("flaky: {}", path.display())));
            }
        }
        OsFiles.read(path)
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        if let Err(e) = self.step("write", path) {
            OsFiles.write(path, &bytes[..bytes.len() / 2])?;
            return Err(e);
        }
        OsFiles.write(path, bytes)
    }

    fn sync_file(&self, path: &Path) -> io::Result<()> {
        self.step("sync_file", path)?;
        OsFiles.sync_file(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.step("rename", from)?;
        {
            let mut s = self.state.lock().unwrap();
            if s.locked.iter().any(|p| p == from || p == to) {
                return Err(injected(format!("locked: {}", from.display())));
            }
            if s.rename_to.as_deref() == Some(to) {
                s.rename_to = None;
                return Err(injected(format!("rename onto {} failed", to.display())));
            }
        }
        OsFiles.rename(from, to)
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        self.step("remove", path)?;
        OsFiles.remove(path)
    }

    fn exists(&self, path: &Path) -> io::Result<bool> {
        self.step("exists", path)?;
        OsFiles.exists(path)
    }

    fn list(&self, dir: &Path) -> io::Result<Entries> {
        self.step("list", dir)?;
        OsFiles.list(dir)
    }

    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        self.step("sync_dir", dir)?;
        OsFiles.sync_dir(dir)
    }

    fn pause(&self) {
        self.state.lock().unwrap().pauses += 1;
    }
}

/// A store in a fresh directory whose file operations `Faulty` controls.
pub(super) fn faulty_store() -> (tempfile::TempDir, Arc<Faulty>, Store) {
    let dir = tempfile::tempdir().unwrap();
    let faulty = Arc::new(Faulty::default());
    let store = reopen(&dir.path().join("state"), &faulty);
    (dir, faulty, store)
}

/// A new store on `dir` through `faulty`: the next process (a reboot),
/// which knows nothing of what the previous one wrote.
pub(super) fn reopen(dir: &Path, faulty: &Arc<Faulty>) -> Store {
    let files: Arc<dyn Files> = faulty.clone();
    Store::with_files(dir, files).unwrap()
}

#[test]
fn a_save_never_writes_save_tmp_in_place() {
    // #32 P1: save.tmp may hold the only copy of the newest state (an
    // interrupted save the recovery could not finish). A later save that
    // fails while writing (a full disk, a crash) must not cut it off.
    for mode in [Mode::Once, Mode::From] {
        let (_d, faulty, s) = faulty_store();
        s.save(&sample(5)).unwrap();
        fs::write(s.dir().join(TMP), encode(&sample(6)).unwrap()).unwrap();
        faulty.arm(0, mode);
        assert!(s.save(&sample(7)).is_err());
        faulty.count();
        let loaded = s.load(&test_site());
        assert_eq!(loaded.persisted.rev, 6, "{mode:?}");
        assert_eq!(loaded.source, Source::Interrupted, "{mode:?}");
    }
}

fn rev_of(path: &Path) -> u64 {
    decode(&fs::read(path).unwrap()).unwrap().rev
}

/// `saved_unix_ms` of the session's saves in these tests: tells them apart
/// from `sample`'s at the same revision.
const SESSION: u64 = 42;

/// The running engine's state at `rev`, told apart from `sample(rev)`.
fn session(rev: u64) -> Persisted {
    let mut p = sample(rev);
    p.saved_unix_ms = SESSION;
    p
}

/// Whether some file in `dir` holds exactly `bytes`.
fn kept(dir: &Path, bytes: &[u8]) -> bool {
    fs::read_dir(dir)
        .unwrap()
        .any(|e| fs::read(e.unwrap().path()).is_ok_and(|b| b == bytes))
}

// ---- a save.tmp this store did not write (#32 final review, MAJOR-1) ----

/// current.json at 5 and, left by a crashed save, save.tmp at 6, which the
/// next process's boot cannot read (another process holds it a moment):
/// the store, its pending bytes.
fn unread_save_tmp() -> (tempfile::TempDir, Arc<Faulty>, Store, Vec<u8>) {
    let (d, faulty, s) = faulty_store();
    s.save(&sample(5)).unwrap();
    let pending = encode(&sample(6)).unwrap();
    fs::write(s.dir().join(TMP), &pending).unwrap();
    let s = reopen(s.dir(), &faulty);
    faulty.set_unreadable(&s.dir().join(TMP), true);
    let boot = s.load(&test_site());
    assert_eq!(
        (boot.source, boot.persisted.rev, boot.save_tmp),
        (Source::Current, 5, FileState::Unreadable)
    );
    s.recover(&boot);
    faulty.set_unreadable(&s.dir().join(TMP), false);
    (d, faulty, s, pending)
}

#[test]
fn a_save_moves_a_save_tmp_it_did_not_write_aside() {
    // The review's MAJOR-1: the session's save renamed save.new over a
    // save.tmp the boot could not read, the newest state lost.
    let g = test_site();
    let (_d, faulty, s, pending) = unread_save_tmp();
    let committed = s.save(&session(6)).unwrap();
    let orphan = s.dir().join("save.tmp.orphan-1");
    assert_eq!(committed.orphaned, Some(orphan.clone()));
    assert_eq!(fs::read(&orphan).unwrap(), pending);
    // An orphan is kept for inspection and never loaded: the session's
    // saves are what the band hears.
    let again = reopen(s.dir(), &faulty).load(&g);
    assert_eq!(
        (again.source, again.persisted.saved_unix_ms),
        (Source::Current, SESSION)
    );
    assert!(again.alarms.is_empty(), "{:?}", again.alarms);
    // The store's own save.tmp (a save whose commit failed) is replaced
    // without a move aside.
    faulty.fail_next("exists");
    assert!(s.save(&session(7)).is_err());
    assert_eq!(rev_of(&s.dir().join(TMP)), 7);
    assert_eq!(s.save(&session(8)).unwrap().orphaned, None);
    assert!(!s.dir().join("save.tmp.orphan-2").exists());
    assert_eq!(rev_of(&s.dir().join(CURRENT)), 8);
}

#[test]
fn a_boot_past_a_locked_current_json_keeps_the_sessions_edits() {
    // The review's MAJOR-3: current.json at 100 is locked at boot, so
    // generation 1 (97) loads. The session edits and saves while
    // current.json stays locked (it cannot become a generation, so each
    // save fails with its state in save.tmp). Once current.json can be
    // read again, the session's newest save must win over it: the load
    // continued the revision 1 000 000 above the highest one the state
    // loaded or a name shows (current.json's 100, by its marker; F3 round
    // 4, finding 2).
    let g = test_site();
    let (_d, faulty, s) = faulty_store();
    s.save(&sample(97)).unwrap();
    s.save(&sample(100)).unwrap();
    let current = s.dir().join(CURRENT);
    let s = reopen(s.dir(), &faulty);
    faulty.set_locked(&current, true);
    let boot = s.load(&g);
    assert_eq!(
        (boot.source, boot.persisted.rev),
        (Source::Generation(1), 1_000_100)
    );
    assert!(
        boot.alarms
            .iter()
            .any(|a| a.contains("the revision continues at 1000100")),
        "{:?}",
        boot.alarms
    );
    assert!(s.recover(&boot).failed.is_empty());
    // Two edits, then the save.
    let edited = boot.persisted.rev + 2;
    assert!(s.save(&session(edited)).is_err());
    faulty.set_locked(&current, false);
    let s = reopen(s.dir(), &faulty);
    let again = s.load(&g);
    assert_eq!(
        (
            again.source,
            again.persisted.rev,
            again.persisted.saved_unix_ms
        ),
        (Source::Interrupted, edited, SESSION)
    );
    assert!(again.alarms.is_empty(), "{:?}", again.alarms);
    // F3 round 4, finding 2 (b), the chained jump: that boot finishes the
    // interrupted save, so the old current.json (100) becomes a generation
    // and current.json holds the jumped revision. The engine then ends
    // without a shutdown save, and the next boot cannot read current.json
    // again: the newest generation it loads holds 100, far below. Its
    // revision continues above what the names show (current.json's, by
    // its marker), or the session's next save loses to that file once it
    // can be read.
    assert!(s.recover(&again).finished);
    faulty.set_locked(&current, true);
    let s = reopen(s.dir(), &faulty);
    let boot = s.load(&g);
    assert_eq!(
        (boot.source, boot.persisted.rev),
        (Source::Generation(2), 2_000_102)
    );
    assert!(s.recover(&boot).failed.is_empty());
    let edited = boot.persisted.rev + 1;
    assert!(s.save(&session(edited)).is_err());
    faulty.set_locked(&current, false);
    let again = reopen(s.dir(), &faulty).load(&g);
    assert_eq!(
        (
            again.source,
            again.persisted.rev,
            again.persisted.saved_unix_ms
        ),
        (Source::Interrupted, 2_000_103, SESSION)
    );
    assert!(again.alarms.is_empty(), "{:?}", again.alarms);
}

#[test]
fn a_save_at_the_revision_the_marker_shows_leaves_the_marker_alone() {
    // #32 F3-r4 2: a save without a change (the same revision) has nothing
    // to rename, so a marker another process holds a moment cannot fail it;
    // a save at another revision needs the marker and fails with the new
    // state in save.tmp.
    let (_d, faulty, s) = faulty_store();
    s.save(&sample(5)).unwrap();
    let mark = s.dir().join("current.json.rev-5");
    faulty.set_locked(&mark, true);
    assert_eq!(s.save(&sample(5)).unwrap().generation, 1);
    assert!(s.save(&sample(6)).is_err());
    assert_eq!(rev_of(&s.dir().join(TMP)), 6);
    faulty.set_locked(&mark, false);
    let loaded = s.load(&test_site());
    assert_eq!(
        (loaded.source, loaded.persisted.rev),
        (Source::Interrupted, 6)
    );
}

#[test]
fn a_boot_on_muted_defaults_past_an_unreadable_current_json_continues_above_its_name() {
    // F3 round 4, finding 2 (a): nothing else loads, so the muted defaults
    // (revision 0) jumped to 1 000 000 only, below a current.json past a
    // million revisions; the session's save then lost to it.
    let g = test_site();
    let (_d, faulty, s) = faulty_store();
    s.save(&sample(1_500_000)).unwrap();
    let current = s.dir().join(CURRENT);
    let s = reopen(s.dir(), &faulty);
    faulty.set_locked(&current, true);
    let boot = s.load(&g);
    assert_eq!(
        (boot.source, boot.persisted.rev),
        (Source::Defaults, 2_500_000)
    );
    assert!(s.recover(&boot).failed.is_empty());
    assert!(s.save(&session(2_500_001)).is_err());
    faulty.set_locked(&current, false);
    let again = reopen(s.dir(), &faulty).load(&g);
    assert_eq!(
        (
            again.source,
            again.persisted.rev,
            again.persisted.saved_unix_ms
        ),
        (Source::Interrupted, 2_500_001, SESSION)
    );
    assert!(again.alarms.is_empty(), "{:?}", again.alarms);
}

#[test]
fn a_boot_past_an_unreadable_newer_generation_continues_above_its_name() {
    // F3 round 4, finding 2 (c): with current.json missing (or damaged)
    // the chain loads the newest valid generation. One newer than it that
    // cannot be read may hold any revision up to what its name shows, and
    // gave no jump, so the session's next save lost to it once readable.
    let g = test_site();
    let (_d, faulty, s) = faulty_store();
    for rev in [40, 50, 60] {
        s.save(&sample(rev)).unwrap();
    }
    fs::remove_file(s.dir().join(CURRENT)).unwrap();
    // The marker (current.json held 60) goes too: only the generation's
    // own name shows its revision.
    fs::remove_file(s.dir().join("current.json.rev-60")).unwrap();
    let newer = s.generations().unwrap()[1].1.clone();
    assert!(
        newer.ends_with("gen-0000000002-r50.json"),
        "{}",
        newer.display()
    );
    let s = reopen(s.dir(), &faulty);
    faulty.set_unreadable(&newer, true);
    let boot = s.load(&g);
    assert_eq!(
        (boot.source, boot.persisted.rev),
        (Source::Generation(1), 1_000_050)
    );
    let note = "gen-0000000002-r50.json cannot be read, so the revision continues at 1000050";
    assert!(
        boot.alarms.iter().any(|a| a.starts_with(note)),
        "{:?}",
        boot.alarms
    );
    assert!(s.recover(&boot).failed.is_empty());
    // The session's next save is cut off before current.json (a crash).
    let next = encode(&session(boot.persisted.rev + 1)).unwrap();
    fs::write(s.dir().join(TMP), next).unwrap();
    faulty.set_unreadable(&newer, false);
    let again = reopen(s.dir(), &faulty).load(&g);
    assert_eq!(
        (
            again.source,
            again.persisted.rev,
            again.persisted.saved_unix_ms
        ),
        (Source::Interrupted, 1_000_051, SESSION)
    );
    assert!(again.alarms.is_empty(), "{:?}", again.alarms);
}

#[test]
fn a_save_tmp_that_cannot_be_moved_aside_safely_fails_the_save() {
    // The move aside is flushed before save.new may take save.tmp's name;
    // if that fails, the save fails and nothing is lost.
    let (_d, faulty, s, pending) = unread_save_tmp();
    faulty.fail_next("sync_dir");
    assert!(s.save(&session(6)).is_err());
    assert!(kept(s.dir(), &pending));
    assert_eq!(rev_of(&s.dir().join(CURRENT)), 5);
    // Still held by another process: it cannot be moved, the save fails.
    let (_d, faulty, s, pending) = unread_save_tmp();
    faulty.set_locked(&s.dir().join(TMP), true);
    assert!(s.save(&session(6)).is_err());
    faulty.set_locked(&s.dir().join(TMP), false);
    assert_eq!(fs::read(s.dir().join(TMP)).unwrap(), pending);
    assert_eq!(rev_of(&s.dir().join(CURRENT)), 5);
}

#[test]
fn a_save_cut_off_after_its_move_aside_names_the_orphan_and_the_boot_alarms() {
    // F3 round 4, finding 1: the rename of save.new over save.tmp failed
    // after the move aside, and neither its error nor any boot named the
    // orphan, which may hold the newest pending state.
    let (_d, faulty, s, pending) = unread_save_tmp();
    let orphan = s.dir().join("save.tmp.orphan-1");
    faulty.fail_rename_to(&s.dir().join(TMP));
    let e = s.save(&session(6)).unwrap_err();
    assert!(e.to_string().contains(&orphan.display().to_string()), "{e}");
    assert_eq!(fs::read(&orphan).unwrap(), pending);
    // A crash now: the next boot loads current.json (5), and an alarm names
    // the orphan above it.
    let boot = reopen(s.dir(), &faulty).load(&test_site());
    assert_eq!((boot.source, boot.persisted.rev), (Source::Current, 5));
    assert_eq!(
        boot.alarms,
        [
            "save.tmp.orphan-1 (revision 6) is above the state loaded (revision 5): \
          a save.tmp moved aside, kept but never loaded"
        ]
    );
}

#[test]
fn every_save_tmp_the_boot_did_not_load_is_moved_aside_by_the_next_save() {
    // Older than current.json (a leftover) or damaged: not the loaded
    // state, so not the store's to replace.
    let g = test_site();
    for (what, bytes) in [
        ("older", encode(&sample(4)).unwrap()),
        ("damaged", b"cut off".to_vec()),
    ] {
        let (_d, faulty, s) = faulty_store();
        s.save(&sample(5)).unwrap();
        fs::write(s.dir().join(TMP), &bytes).unwrap();
        let s = reopen(s.dir(), &faulty);
        let boot = s.load(&g);
        assert_eq!(boot.source, Source::Current, "{what}");
        s.recover(&boot);
        let orphan = s.dir().join("save.tmp.orphan-1");
        assert_eq!(
            s.save(&session(6)).unwrap().orphaned,
            Some(orphan.clone()),
            "{what}"
        );
        assert_eq!(fs::read(&orphan).unwrap(), bytes, "{what}");
    }
    // The loaded save.tmp is the store's: its recovery could not finish,
    // the next save replaces it (the session's state includes it).
    let (_d, faulty, s) = faulty_store();
    s.save(&sample(5)).unwrap();
    fs::write(s.dir().join(TMP), encode(&sample(6)).unwrap()).unwrap();
    let s = reopen(s.dir(), &faulty);
    let boot = s.load(&g);
    assert_eq!(boot.source, Source::Interrupted);
    faulty.fail_next("sync_file");
    assert!(!s.recover(&boot).finished);
    assert_eq!(s.save(&session(7)).unwrap().orphaned, None);
    assert_eq!(rev_of(&s.dir().join(CURRENT)), 7);
}

#[test]
fn a_pruning_failure_leaves_the_save_committed() {
    // #32 P6: generations are pruned after the commit; a failed removal
    // is reported apart and never makes a committed save a failure.
    let g = test_site();
    let (_d, faulty, s) = faulty_store();
    for rev in 1..=21 {
        s.save(&sample(rev)).unwrap();
    }
    faulty.fail_next("remove");
    let committed = s.save(&sample(22)).unwrap();
    assert_eq!(committed.generation, 21);
    assert!(committed.pruning.is_some());
    assert_eq!(s.load(&g).persisted.rev, 22);
    // Nor does it make a recovery unfinished.
    let (_d, faulty, s) = faulty_store();
    for rev in 1..=21 {
        s.save(&sample(rev)).unwrap();
    }
    fs::write(s.dir().join(TMP), encode(&sample(22)).unwrap()).unwrap();
    let loaded = s.load(&g);
    assert_eq!(loaded.source, Source::Interrupted);
    faulty.fail_next("remove");
    let done = s.recover(&loaded);
    assert!(done.finished, "{done:?}");
    assert!(done.failed.is_empty(), "{done:?}");
    assert_eq!(done.warnings.len(), 1, "{done:?}");
    assert_eq!(rev_of(&s.dir().join(CURRENT)), 22);
}

#[test]
fn a_current_json_that_cannot_be_looked_at_is_never_replaced() {
    // #32 P7: an error while asking whether current.json exists is not
    // "absent": the save stops before save.tmp could replace it, and the
    // newest state waits in save.tmp.
    let (_d, faulty, s) = faulty_store();
    s.save(&sample(5)).unwrap();
    faulty.fail_next("exists");
    assert!(s.save(&sample(6)).is_err());
    assert_eq!(rev_of(&s.dir().join(CURRENT)), 5);
    assert_eq!(rev_of(&s.dir().join(TMP)), 6);
    let loaded = s.load(&test_site());
    assert_eq!(
        (loaded.source, loaded.persisted.rev),
        (Source::Interrupted, 6)
    );
}

#[test]
fn a_damaged_current_json_that_cannot_be_moved_aside_becomes_a_skipped_generation() {
    // #32 P3: the move aside fails (a failing rename), the interrupted save
    // is finished anyway; the damaged file lands among the generations,
    // where the chain skips it.
    let g = test_site();
    let (_d, faulty, s) = faulty_store();
    s.save(&sample(7)).unwrap();
    s.save(&sample(8)).unwrap();
    fs::write(s.dir().join(CURRENT), b"damaged").unwrap();
    fs::write(s.dir().join(TMP), encode(&sample(9)).unwrap()).unwrap();
    let loaded = s.load(&g);
    assert_eq!(
        (loaded.source, loaded.current_json),
        (Source::Interrupted, FileState::Damaged)
    );
    faulty.fail_next("rename");
    let done = s.recover(&loaded);
    assert!(done.quarantined.is_none());
    assert!(done.finished, "{done:?}");
    assert_eq!(done.failed.len(), 1, "{done:?}");
    assert_eq!(rev_of(&s.dir().join(CURRENT)), 9);
    let gens = s.generations().unwrap();
    assert_eq!(gens.len(), 2);
    assert_eq!(fs::read(&gens[1].1).unwrap(), b"damaged");
    // Without current.json the chain passes over the damaged generation.
    fs::remove_file(s.dir().join(CURRENT)).unwrap();
    let loaded = s.load(&g);
    assert_eq!(
        (loaded.source, loaded.persisted.rev),
        (Source::Generation(1), 7)
    );
}

#[test]
fn a_move_aside_whose_directory_sync_fails_is_reported_as_moved() {
    // #32 minor-6: the rename went through and only the directory sync
    // after it failed. The damaged file is aside (the report says where
    // and that the sync failed), not "could not be moved aside".
    let (_d, faulty, s) = faulty_store();
    s.save(&sample(7)).unwrap();
    s.save(&sample(8)).unwrap();
    fs::write(s.dir().join(CURRENT), b"damaged").unwrap();
    let s = reopen(s.dir(), &faulty);
    let loaded = s.load(&test_site());
    assert_eq!(loaded.current_json, FileState::Damaged);
    faulty.fail_next("sync_dir");
    let done = s.recover(&loaded);
    let aside = s.dir().join("current.json.damaged-1");
    assert_eq!(done.quarantined, Some(aside.clone()), "{done:?}");
    assert_eq!(fs::read(&aside).unwrap(), b"damaged");
    assert!(!s.dir().join(CURRENT).exists());
    assert_eq!(done.failed.len(), 1, "{done:?}");
    assert!(
        done.failed[0].starts_with(&format!(
            "the damaged current.json was moved aside to {}, but the directory sync failed",
            aside.display()
        )),
        "{done:?}"
    );
}

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

// ---- every single failure (#32 P9) ----

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

/// Runs `op` after `setup` with each single failure it can meet: once (an
/// error, the engine runs on) and from that step on (a crash: the next
/// process boots and recovers). After it, a boot loads the newest
/// committed or pending state without an alarm; the engine then saves its
/// next edit (the boot's revision + 1), which is the newest state on disk
/// and what the next boot loads (#32 minor-8).
fn every_failure<T>(what: &str, setup: impl Fn(&Store) -> T, op: impl Fn(&Store, &T)) {
    let g = test_site();
    let (_d, faulty, s) = faulty_store();
    let prepared = setup(&s);
    faulty.count();
    op(&s, &prepared);
    let steps = faulty.steps();
    assert!(steps > 3, "{what}: {steps} steps");
    for mode in [Mode::Once, Mode::From] {
        for at in 0..steps {
            let (_d, faulty, s) = faulty_store();
            let prepared = setup(&s);
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
            assert!(boot.alarms.is_empty(), "{what}, {mode:?} at {at}: {boot:?}");
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
    every_failure("save", filled, |s, _| {
        let _ = s.save(&sample(22));
    });
}

#[test]
fn a_recovery_survives_every_single_failure() {
    every_failure(
        "recover",
        |s| {
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
        |s| {
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
