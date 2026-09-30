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

    /// The next step of kind `what` ("write", "rename", "remove", …) fails.
    pub(super) fn fail_next(&self, what: &'static str) {
        self.state.lock().unwrap().next = Some(what);
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
            return Err(io::Error::other(format!(
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
}

/// A store in a fresh directory whose file operations `Faulty` controls.
pub(super) fn faulty_store() -> (tempfile::TempDir, Arc<Faulty>, Store) {
    let dir = tempfile::tempdir().unwrap();
    let faulty = Arc::new(Faulty::default());
    let files: Arc<dyn Files> = faulty.clone();
    let store = Store::with_files(&dir.path().join("state"), files).unwrap();
    (dir, faulty, store)
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
