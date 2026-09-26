//! Transactional band output (#20 design note §7). `band` never writes into
//! the band directory itself: it copies the directory into a staging
//! directory next to it, writes everything there, syncs it, marks it
//! complete, and swaps it in with two renames (the old directory aside, the
//! staging directory into place). A failure before the swap leaves the band
//! directory exactly as it was; [`recover`] finishes or undoes a swap that a
//! crash interrupted, and runs at the start of every `band` run.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The file that marks a staging directory complete.
pub const MARKER: &str = ".iem-migrate-complete";

/// Where a write is (the fault-injection points of the tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Copying file `n` of the existing band directory.
    Copy(usize),
    /// Writing output item `n`.
    Write(usize),
    /// Marking the staging directory complete.
    Marker,
    /// Moving the band directory aside.
    Aside,
    /// Moving the staging directory into place.
    Swap,
}

/// No faults: what `band` runs with.
pub fn no_faults(_: Step) -> io::Result<()> {
    Ok(())
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

/// The staging and old directories of `target`: siblings named after it.
pub fn siblings(target: &Path) -> io::Result<(PathBuf, PathBuf)> {
    let name = target
        .file_name()
        .ok_or_else(|| invalid(format!("{}: not a directory name", target.display())))?
        .to_string_lossy();
    let parent = target.parent().unwrap_or_else(|| Path::new(""));
    Ok((
        parent.join(format!(".{name}.iem-migrate")),
        parent.join(format!(".{name}.iem-migrate-old")),
    ))
}

fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut f = File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Windows has no directory handle to flush this way.
#[cfg(not(unix))]
fn sync_dir(_: &Path) -> io::Result<()> {
    Ok(())
}

fn remove_if_there(dir: &Path) -> io::Result<()> {
    match fs::remove_dir_all(dir) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Copies the tree `from` into `to` (which exists), file by file, synced.
fn copy_tree(
    from: &Path,
    to: &Path,
    n: &mut usize,
    fail: &dyn Fn(Step) -> io::Result<()>,
) -> io::Result<()> {
    let mut entries = fs::read_dir(from)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for e in entries {
        let kind = e.file_type()?;
        let dst = to.join(e.file_name());
        if kind.is_dir() {
            fs::create_dir(&dst)?;
            copy_tree(&e.path(), &dst, n, fail)?;
        } else if kind.is_file() {
            fail(Step::Copy(*n))?;
            *n += 1;
            fs::copy(e.path(), &dst)?;
            File::open(&dst)?.sync_all()?;
        } else {
            return Err(invalid(format!(
                "{}: only files and directories belong in the band directory",
                e.path().display()
            )));
        }
    }
    sync_dir(to)
}

/// Finishes or undoes an interrupted swap of `target`:
///
/// - the target exists: a leftover marker is removed (the swap had finished),
///   and any staging or old directory is removed (it is not the target);
/// - the target is missing: a complete staging directory becomes the target,
///   else the old directory does (the swap had not happened); leftovers go.
///
/// Returns what it did, one line each.
pub fn recover(target: &Path) -> io::Result<Vec<String>> {
    let (staging, old) = siblings(target)?;
    let mut done = Vec::new();
    if !target.exists() {
        if staging.join(MARKER).exists() {
            fs::rename(&staging, target)?;
            done.push(format!(
                "recovered: completed the interrupted swap into {}",
                target.display()
            ));
        } else if old.exists() {
            fs::rename(&old, target)?;
            done.push(format!(
                "recovered: restored {} from before an interrupted run",
                target.display()
            ));
        }
    }
    if target.join(MARKER).exists() {
        fs::remove_file(target.join(MARKER))?;
    }
    for leftover in [&staging, &old] {
        if leftover.exists() {
            fs::remove_dir_all(leftover)?;
            done.push(format!("recovered: removed {}", leftover.display()));
        }
    }
    Ok(done)
}

/// A staging directory for `target`.
pub struct Stage {
    target: PathBuf,
    staging: PathBuf,
    old: PathBuf,
    items: usize,
}

impl Stage {
    /// Recovers an interrupted run, then stages a copy of `target` (when it
    /// exists). On a failure nothing is left behind.
    pub fn begin(target: &Path, fail: &dyn Fn(Step) -> io::Result<()>) -> io::Result<Self> {
        recover(target)?;
        let (staging, old) = siblings(target)?;
        fs::create_dir_all(&staging)?;
        let stage = Self {
            target: target.to_path_buf(),
            staging,
            old,
            items: 0,
        };
        if target.exists() {
            let mut n = 0;
            if let Err(e) = copy_tree(target, &stage.staging, &mut n, fail) {
                stage.abort();
                return Err(e);
            }
        }
        Ok(stage)
    }

    /// The directory every output is written into.
    pub fn dir(&self) -> &Path {
        &self.staging
    }

    /// The next output item begins (a fault-injection point).
    pub fn step(&mut self, fail: &dyn Fn(Step) -> io::Result<()>) -> io::Result<()> {
        let n = self.items;
        self.items += 1;
        fail(Step::Write(n))
    }

    /// Writes `bytes` to `rel` inside the staging directory, synced.
    pub fn write(&self, rel: &Path, bytes: &[u8]) -> io::Result<()> {
        let path = self.staging.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        write_synced(&path, bytes)
    }

    /// Drops the staging directory; the target is untouched.
    pub fn abort(self) {
        let _ = fs::remove_dir_all(&self.staging);
    }

    /// Marks the staging directory complete and swaps it in. A failure up
    /// to the swap restores the target; after it, the old copy's removal is
    /// best effort and reported (the next run's [`recover`] removes it).
    pub fn commit(self, fail: &dyn Fn(Step) -> io::Result<()>) -> io::Result<Vec<String>> {
        let aside = self.target.exists();
        let swapped = (|| {
            fail(Step::Marker)?;
            write_synced(&self.staging.join(MARKER), b"")?;
            sync_dir(&self.staging)?;
            if aside {
                fail(Step::Aside)?;
                fs::rename(&self.target, &self.old)?;
            }
            fail(Step::Swap)?;
            fs::rename(&self.staging, &self.target)
        })();
        if let Err(e) = swapped {
            if aside && !self.target.exists() {
                let _ = fs::rename(&self.old, &self.target);
            }
            let _ = fs::remove_file(self.target.join(MARKER));
            self.abort();
            return Err(e);
        }
        let mut notes = Vec::new();
        if let Some(parent) = self.target.parent().filter(|p| !p.as_os_str().is_empty()) {
            let _ = sync_dir(parent);
        }
        if let Err(e) = remove_if_there(&self.old) {
            notes.push(format!(
                "note: {} was not removed ({e}); the next run removes it",
                self.old.display()
            ));
        }
        if let Err(e) = fs::remove_file(self.target.join(MARKER)) {
            notes.push(format!(
                "note: {} was not removed ({e}); the next run removes it",
                self.target.join(MARKER).display()
            ));
        }
        Ok(notes)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::BTreeMap;

    use super::*;

    /// Every file under `dir` (relative path → bytes); `None` when absent.
    fn tree(dir: &Path) -> Option<BTreeMap<String, Vec<u8>>> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            for e in fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(root, &p, out);
                } else {
                    let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
                    out.insert(rel, fs::read(&p).unwrap());
                }
            }
        }
        if !dir.exists() {
            return None;
        }
        let mut out = BTreeMap::new();
        walk(dir, dir, &mut out);
        Some(out)
    }

    fn fill(dir: &Path, files: &[(&str, &str)]) {
        for (rel, text) in files {
            let p = dir.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }
    }

    fn leftovers(target: &Path) -> (bool, bool) {
        let (staging, old) = siblings(target).unwrap();
        (staging.exists(), old.exists())
    }

    /// One band run: stage, write two items, commit.
    fn run(target: &Path, fail: &dyn Fn(Step) -> io::Result<()>) -> io::Result<Vec<String>> {
        let mut s = Stage::begin(target, fail)?;
        let written = (|| {
            s.step(fail)?;
            s.write(Path::new("presets/member1.json"), b"new presets")?;
            s.step(fail)?;
            s.write(Path::new("cert.pem"), b"new cert")
        })();
        match written {
            Ok(()) => s.commit(fail),
            Err(e) => {
                s.abort();
                Err(e)
            }
        }
    }

    #[test]
    fn siblings_sit_next_to_the_target() {
        let (s, o) = siblings(Path::new("/srv/iem/band")).unwrap();
        assert_eq!(s, PathBuf::from("/srv/iem/.band.iem-migrate"));
        assert_eq!(o, PathBuf::from("/srv/iem/.band.iem-migrate-old"));
        let (s, _) = siblings(Path::new("band")).unwrap();
        assert_eq!(s, PathBuf::from(".band.iem-migrate"));
        assert!(siblings(Path::new("/")).is_err());
        assert!(no_faults(Step::Swap).is_ok());
    }

    #[test]
    fn a_commit_swaps_in_the_staged_copy_with_the_new_files() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("band");
        fill(
            &target,
            &[
                ("secrets/pin_hashes.json", "old pins"),
                ("cert.pem", "old cert"),
            ],
        );
        let notes = run(&target, &no_faults).unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        let t = tree(&target).unwrap();
        assert_eq!(t["secrets/pin_hashes.json"], b"old pins");
        assert_eq!(t["cert.pem"], b"new cert");
        assert_eq!(t["presets/member1.json"], b"new presets");
        assert_eq!(t.len(), 3, "no marker is left: {t:?}");
        assert_eq!(leftovers(&target), (false, false));
    }

    #[test]
    fn a_first_commit_creates_the_target() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("band");
        run(&target, &no_faults).unwrap();
        assert_eq!(tree(&target).unwrap().len(), 2);
        assert_eq!(leftovers(&target), (false, false));
    }

    #[test]
    fn a_failure_at_every_step_leaves_the_target_as_it_was() {
        for existing in [true, false] {
            let mut steps = Vec::new();
            for k in 0.. {
                let d = tempfile::tempdir().unwrap();
                let target = d.path().join("band");
                if existing {
                    fill(
                        &target,
                        &[
                            ("a.json", "a"),
                            ("sub/b.json", "b"),
                            ("cert.pem", "old cert"),
                        ],
                    );
                }
                let before = tree(&target);
                let calls = Cell::new(0usize);
                let seen = Cell::new(None);
                let fail = |s: Step| {
                    let n = calls.get();
                    calls.set(n + 1);
                    if n == k {
                        seen.set(Some(s));
                        Err(io::Error::other(format!("injected at {s:?}")))
                    } else {
                        Ok(())
                    }
                };
                match run(&target, &fail) {
                    Err(e) => {
                        assert!(e.to_string().starts_with("injected"), "{e}");
                        assert_eq!(tree(&target), before, "step {:?}", seen.get());
                        assert_eq!(leftovers(&target), (false, false), "step {:?}", seen.get());
                        steps.extend(seen.get());
                    }
                    Ok(_) => {
                        assert_eq!(tree(&target).unwrap()["cert.pem"], b"new cert");
                        break;
                    }
                }
            }
            let mut want: Vec<Step> = if existing {
                (0..3).map(Step::Copy).collect()
            } else {
                Vec::new()
            };
            want.extend([Step::Write(0), Step::Write(1), Step::Marker]);
            if existing {
                want.push(Step::Aside);
            }
            want.push(Step::Swap);
            assert_eq!(steps, want, "existing {existing}");
        }
    }

    #[test]
    fn an_io_error_while_writing_leaves_the_target_as_it_was() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("band");
        fill(&target, &[("a.json", "a")]);
        let before = tree(&target);
        let mut s = Stage::begin(&target, &no_faults).unwrap();
        // A directory where the file belongs: the write fails.
        fs::create_dir_all(s.dir().join("cert.pem")).unwrap();
        s.step(&no_faults).unwrap();
        assert!(s.write(Path::new("cert.pem"), b"x").is_err());
        s.abort();
        assert_eq!(tree(&target), before);
        assert_eq!(leftovers(&target), (false, false));
    }

    #[test]
    fn recover_rolls_forward_a_complete_swap_and_back_an_incomplete_one() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("band");
        let (staging, old) = siblings(&target).unwrap();
        let reset = || {
            for p in [&target, &staging, &old] {
                let _ = fs::remove_dir_all(p);
            }
        };
        let staged = |marker: bool| {
            fill(&staging, &[("x.json", "new")]);
            if marker {
                fill(&staging, &[(MARKER, "")]);
            }
        };
        // A crash after the aside, before the swap: complete → forward.
        reset();
        fill(&old, &[("x.json", "old")]);
        staged(true);
        let done = recover(&target).unwrap();
        assert_eq!(tree(&target).unwrap()["x.json"], b"new");
        assert!(!target.join(MARKER).exists());
        assert_eq!(leftovers(&target), (false, false));
        assert_eq!(done.len(), 2, "{done:?}");
        // The same without the marker: back to the old copy.
        reset();
        fill(&old, &[("x.json", "old")]);
        staged(false);
        recover(&target).unwrap();
        assert_eq!(tree(&target).unwrap()["x.json"], b"old");
        assert_eq!(leftovers(&target), (false, false));
        // Only the old copy: it is the target again.
        reset();
        fill(&old, &[("x.json", "old")]);
        recover(&target).unwrap();
        assert_eq!(tree(&target).unwrap()["x.json"], b"old");
        // A crash after the swap, before the clean-up.
        reset();
        fill(&target, &[("x.json", "new"), (MARKER, "")]);
        fill(&old, &[("x.json", "old")]);
        recover(&target).unwrap();
        assert_eq!(tree(&target).unwrap().len(), 1);
        assert_eq!(leftovers(&target), (false, false));
        // A crash before the aside: the target is intact, the staging goes.
        for marker in [true, false] {
            reset();
            fill(&target, &[("x.json", "old")]);
            staged(marker);
            recover(&target).unwrap();
            assert_eq!(tree(&target).unwrap()["x.json"], b"old");
            assert_eq!(leftovers(&target), (false, false));
        }
        // A first run: a complete staging becomes the target, else it goes.
        reset();
        staged(true);
        recover(&target).unwrap();
        assert_eq!(tree(&target).unwrap()["x.json"], b"new");
        reset();
        staged(false);
        recover(&target).unwrap();
        assert_eq!(tree(&target), None);
        assert_eq!(leftovers(&target), (false, false));
        // Nothing to do.
        reset();
        assert!(recover(&target).unwrap().is_empty());
    }

    #[test]
    fn a_run_after_a_crash_finishes_it_first() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("band");
        let (staging, old) = siblings(&target).unwrap();
        fill(&old, &[("kept.json", "from the crashed run")]);
        fill(
            &staging,
            &[("kept.json", "from the crashed run"), (MARKER, "")],
        );
        run(&target, &no_faults).unwrap();
        let t = tree(&target).unwrap();
        assert_eq!(t["kept.json"], b"from the crashed run");
        assert_eq!(t["cert.pem"], b"new cert");
        assert_eq!(leftovers(&target), (false, false));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_in_the_band_directory_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("band");
        fill(&target, &[("a.json", "a")]);
        std::os::unix::fs::symlink(target.join("a.json"), target.join("link.json")).unwrap();
        let e = Stage::begin(&target, &no_faults).err().unwrap();
        assert!(e.to_string().contains("only files and directories"), "{e}");
        assert_eq!(leftovers(&target), (false, false));
    }
}
