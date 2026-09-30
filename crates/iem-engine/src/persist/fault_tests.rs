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
        if self
            .state
            .lock()
            .unwrap()
            .locked
            .iter()
            .any(|p| p == from || p == to)
        {
            return Err(injected(format!("locked: {}", from.display())));
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
    assert_eq!(
        (loaded.source, loaded.persisted.rev),
        (Source::Interrupted, 9)
    );
    assert_eq!(loaded.current_json, FileState::Unreadable);
    assert_eq!(faulty.pauses(), TRIES - 1);
    assert_eq!(loaded.alarms.len(), 1, "{:?}", loaded.alarms);
    assert!(
        loaded.alarms[0].starts_with("current.json cannot be read"),
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
    // A generation passed over while looking for the newest valid one.
    let (_d, faulty, s) = faulty_store();
    for rev in 7..=9 {
        s.save(&sample(rev)).unwrap();
    }
    fs::remove_file(s.dir().join(CURRENT)).unwrap();
    faulty.set_unreadable(&s.dir().join("gen-0000000002.json"), true);
    let loaded = s.load(&g);
    assert_eq!(
        (loaded.source, loaded.persisted.rev),
        (Source::Generation(1), 7)
    );
    assert_eq!(loaded.alarms.len(), 1, "{:?}", loaded.alarms);
    assert!(
        loaded.alarms[0].starts_with("gen-0000000002.json cannot be read"),
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
/// error, the engine runs on and saves `next`) and from that step on (a
/// crash: the next boot recovers, then saves `next`). After it, a boot
/// loads the newest committed or pending state; after the save, `next`.
fn every_failure<T>(what: &str, setup: impl Fn(&Store) -> T, op: impl Fn(&Store, &T), next: u64) {
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
            s.save(&sample(next)).unwrap();
            let again = s.load(&g);
            assert_eq!(
                (again.source, again.persisted.rev),
                (Source::Current, next),
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
        filled,
        |s, _| {
            let _ = s.save(&sample(22));
        },
        23,
    );
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
        23,
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
        23,
    );
}

#[test]
fn a_boot_whose_reads_fail_never_rolls_back_silently() {
    // A read that fails once is tried again; reads that keep failing may
    // load an older state, but never without an alarm (or a fallback
    // source, which the engine alarms on), and leave the files as they are.
    let g = test_site();
    let setup = |s: &Store| {
        filled(s);
        fs::write(s.dir().join(TMP), encode(&sample(22)).unwrap()).unwrap();
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
        }
    }
}
