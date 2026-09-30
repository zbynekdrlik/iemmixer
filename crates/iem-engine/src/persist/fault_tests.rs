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
    }

    fn step(&self, what: &str, path: &Path) -> io::Result<()> {
        let mut s = self.state.lock().unwrap();
        let at = s.steps;
        s.steps += 1;
        let fails = match s.fault {
            Some((n, Mode::Once)) => at == n,
            Some((n, Mode::From)) => at >= n,
            None => false,
        };
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
