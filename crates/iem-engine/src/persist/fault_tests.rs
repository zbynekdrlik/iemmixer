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
    /// Paths a rename FROM fails while a rename onto them succeeds: a move
    /// aside alone fails.
    pinned: Vec<PathBuf>,
    /// A path whose reads beyond the bound panic, and its reads so far.
    read_limit: Option<(PathBuf, usize)>,
    limited_reads: usize,
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

    /// While `pinned`, a rename FROM `path` fails but a rename onto it
    /// succeeds: only a save's own refusal keeps the file then.
    pub(super) fn set_pinned(&self, path: &Path, pinned: bool) {
        let mut s = self.state.lock().unwrap();
        s.pinned.retain(|p| p != path);
        if pinned {
            s.pinned.push(path.to_path_buf());
        }
    }

    /// A read of `path` beyond `limit` (from now) panics at once: a read
    /// tried again without an end fails its test in milliseconds instead
    /// of hanging it (`.claude/rules/engine.md`, tests that wait).
    pub(super) fn limit_reads(&self, path: &Path, limit: usize) {
        let mut s = self.state.lock().unwrap();
        s.read_limit = Some((path.to_path_buf(), limit));
        s.limited_reads = 0;
    }

    /// Reads of the path `limit_reads` names since it was set.
    pub(super) fn limited_reads(&self) -> usize {
        self.state.lock().unwrap().limited_reads
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
            if let Some((limited, limit)) = s.read_limit.clone()
                && limited == path
            {
                s.limited_reads += 1;
                let reads = s.limited_reads;
                assert!(
                    reads <= limit,
                    "{} read {reads} times: a read tried again without an end",
                    path.display()
                );
            }
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
            if s.pinned.iter().any(|p| p == from) {
                return Err(injected(format!("pinned: {}", from.display())));
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
fn an_orphan_above_the_state_loaded_is_named_after_a_revision_jump_too() {
    // Lane G4 review, finding 2: the orphan check compared each orphan
    // with the jumped revision (a million above), so past an unreadable
    // current.json an orphan holding a state above the one that loaded (a
    // save cut off right after its move aside) was never named. It is
    // compared with the state loaded's own revision.
    let g = test_site();
    let (_d, faulty, s) = faulty_store();
    s.save(&sample(97)).unwrap();
    s.save(&sample(100)).unwrap();
    let orphan = s.dir().join("save.tmp.orphan-1");
    fs::write(&orphan, encode(&sample(101)).unwrap()).unwrap();
    let s = reopen(s.dir(), &faulty);
    faulty.set_locked(&s.dir().join(CURRENT), true);
    let boot = s.load(&g);
    assert_eq!(
        (boot.source, boot.persisted.rev),
        (Source::Generation(1), 1_000_100)
    );
    assert!(
        boot.alarms.iter().any(|a| a
            == "save.tmp.orphan-1 (revision 101) is above the state loaded (revision 97): \
                a save.tmp moved aside, kept but never loaded"),
        "{:?}",
        boot.alarms
    );
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
    // It cannot be moved, though a rename onto it would go through (F3
    // round 4, finding 7: a save.tmp locked both ways kept itself, so this
    // half held even without the move aside). The save fails rather than
    // replace it.
    let (_d, faulty, s, pending) = unread_save_tmp();
    faulty.set_pinned(&s.dir().join(TMP), true);
    assert!(s.save(&session(6)).is_err());
    faulty.set_pinned(&s.dir().join(TMP), false);
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

mod reads;
mod sweeps;
