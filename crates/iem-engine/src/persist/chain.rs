//! The load chain (#32): which file holds the state (`current.json`, an
//! interrupted save in `save.tmp`, a generation, the baseline), what the
//! seed must keep (`Store::live_state`), and the boot's recovery that
//! normalizes the directory to what was loaded (`Store::recover`).
//!
//! `save` writes the new state to `save.tmp` (synced), then renames
//! `current.json` to a generation and `save.tmp` to `current.json`. A crash
//! before the second rename leaves the newest state only in `save.tmp`. It
//! is the live state when it passes `decode` and its revision is at least
//! that of the file it competes with ([`supersedes`]): a valid
//! `current.json`, or the newest valid generation when `current.json` is
//! missing or damaged. At boot `recover` moves a damaged `current.json`
//! aside for good and finishes such a save, so `current.json` holds the
//! loaded state before the engine writes anything.

use super::*;

/// Names tried for a damaged `current.json` moved aside:
/// `current.json.damaged-1` up to this.
const QUARANTINE_NAMES: u32 = 1000;

/// Reads of a file that fail with an I/O error, the file pausing between
/// them, before it counts as unreadable (#32 P2).
const READ_TRIES: usize = 5;

impl Store {
    /// The live state the seed must keep, if any: the file the load chain
    /// would use among `current.json`, `save.tmp` and the generations (a
    /// seed is not live state, so `baseline.json` never counts). A file that
    /// exists but does not decode is nothing the engine could load, so it
    /// does not count (the seed's save renames a damaged `current.json`
    /// into a generation, its bytes kept). An I/O error while looking is an
    /// error, never "no state" (#32 D5, m3: the seed would write over state
    /// it could not see).
    pub fn live_state(&self) -> io::Result<Option<Source>> {
        Ok(self
            .pick_live(&mut Pick::default(), Reading::Strict)?
            .map(|(_, source)| source))
    }

    /// The load chain: the live state (`current.json`, `save.tmp` or the
    /// newest valid generation, see the module docs), else `baseline.json`,
    /// else defaults with every mix muted. A file must pass `decode`
    /// (format, schema, the payload's SHA-256, then the payload's parse) to
    /// be used; one that exists and does not, or cannot be read, is
    /// `rejected` with the reason, as is a `save.tmp` passed over for an
    /// older revision and a directory whose generations cannot be listed.
    pub fn load(&self, topo: &Topology) -> Loaded {
        let mut pick = Pick::default();
        let live = self
            .pick_live(&mut pick, Reading::Tolerant)
            .unwrap_or_else(|e| {
                pick.rejected.push((self.dir.clone(), e.to_string()));
                None
            });
        if let Some((persisted, source)) = live {
            return settle(topo, persisted, source, pick);
        }
        let baseline = self.dir.join(BASELINE);
        if let Ok(Read::Valid(persisted)) = self.read_state(&baseline, &mut pick, Reading::Tolerant)
        {
            return settle(topo, persisted, Source::Baseline, pick);
        }
        Loaded {
            persisted: Persisted {
                topology_hash: topo.hash.clone(),
                state: defaults_muted(topo),
                ..Persisted::default()
            },
            source: Source::Defaults,
            rejected: pick.rejected,
            dropped: Vec::new(),
            alarms: pick.alarms,
            current_json: pick.current_json,
        }
    }

    /// The live state and its source, shared by `load` (tolerant: a file it
    /// cannot read is `rejected`) and `live_state` (strict: that is an
    /// error), so the seed names exactly the file the engine loads.
    fn pick_live(
        &self,
        pick: &mut Pick,
        reading: Reading,
    ) -> io::Result<Option<(Persisted, Source)>> {
        let tmp_path = self.dir.join(TMP);
        let current = self.read_state(&self.dir.join(CURRENT), pick, reading)?;
        pick.current_json = current.state();
        let tmp = self.read_state(&tmp_path, pick, reading)?;
        if let Read::Valid(current) = current {
            return Ok(Some(match tmp {
                Read::Valid(tmp) if supersedes(&tmp, &current) => (tmp, Source::Interrupted),
                Read::Valid(tmp) => {
                    pick.rejected.push((
                        tmp_path,
                        format!(
                            "revision {} is not newer than current.json's {}",
                            tmp.rev, current.rev
                        ),
                    ));
                    (current, Source::Current)
                }
                Read::Missing | Read::Unreadable | Read::Damaged => (current, Source::Current),
            }));
        }
        // current.json missing or damaged (#32 m1): save.tmp against the
        // newest valid generation.
        let mut unlisted = None;
        let gens = match self.generations() {
            Ok(gens) => gens,
            Err(e) if reading == Reading::Strict => return Err(e),
            Err(e) => {
                pick.rejected.push((
                    self.dir.clone(),
                    format!("the generations cannot be listed: {e}"),
                ));
                unlisted = Some(e);
                Vec::new()
            }
        };
        let mut newest = None;
        for (seq, path) in gens.into_iter().rev() {
            if let Read::Valid(generation) = self.read_state(&path, pick, reading)? {
                newest = Some((generation, seq));
                break;
            }
        }
        Ok(match (tmp, newest) {
            (Read::Valid(tmp), Some((generation, seq))) if !supersedes(&tmp, &generation) => {
                pick.rejected.push((
                    tmp_path,
                    format!(
                        "revision {} is not newer than generation {seq}'s {}",
                        tmp.rev, generation.rev
                    ),
                ));
                Some((generation, Source::Generation(seq)))
            }
            (Read::Valid(tmp), _) => {
                // #32 P8: compared with nothing when the listing failed.
                if let Some(e) = unlisted {
                    pick.alarms.push(format!(
                        "save.tmp is loaded without comparing it with the \
                         generations, which cannot be listed ({e})"
                    ));
                }
                Some((tmp, Source::Interrupted))
            }
            (_, Some((generation, seq))) => Some((generation, Source::Generation(seq))),
            (_, None) => None,
        })
    }
}

/// What `Store::recover` did to the state directory at boot (#32).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recovery {
    /// Where a damaged `current.json` was moved aside to (never read again).
    pub quarantined: Option<PathBuf>,
    /// The interrupted save was finished: `save.tmp` is `current.json`.
    pub finished: bool,
    /// Steps that failed; the engine runs on the loaded state anyway.
    pub failed: Vec<String>,
    /// What went wrong without undoing anything (old generations left).
    pub warnings: Vec<String>,
}

impl Store {
    /// Normalizes the state directory to `loaded` before the engine runs
    /// (#32 review). A damaged or unreadable `current.json` (one `load`
    /// rejected) is moved aside to the first free `current.json.damaged-<n>`,
    /// a name the load chain never reads, so it cannot come back as a stale
    /// state later. A boot on `save.tmp` then finishes that save the way
    /// `save` would have (`save.tmp` synced, a valid older `current.json`
    /// into the next generation, `save.tmp` to `current.json`, the directory
    /// synced): afterwards `current.json` is the loaded state and no
    /// `save.tmp` remains, so the next save cannot truncate its only copy.
    /// Each step is one rename: a crash in between leaves a layout the next
    /// boot's load chain resolves to the same state, and this finishes it
    /// then. A failed step is reported and the engine runs on the loaded
    /// state anyway. A damaged file that cannot be moved aside does not hold
    /// the save back (#32 P3): the finish then rotates it into a
    /// generation, which is harmless, as the chain skips generations that
    /// do not decode.
    pub fn recover(&self, loaded: &Loaded) -> Recovery {
        let mut done = Recovery::default();
        let current = self.dir.join(CURRENT);
        if loaded.current_json == FileState::Damaged {
            match self.quarantine(&current) {
                Ok(aside) => done.quarantined = Some(aside),
                Err(e) => done.failed.push(format!(
                    "the damaged {CURRENT} could not be moved aside: {e}"
                )),
            }
        }
        if loaded.source == Source::Interrupted && loaded.current_json == FileState::Unreadable {
            // #32 P2: recovery never moves an unreadable file; the runtime's
            // next save replaces save.tmp whole and rotates it then.
            done.warnings.push(format!(
                "the interrupted save stays in {TMP}: {CURRENT} cannot be read, \
                 and recovery never moves it"
            ));
        } else if loaded.source == Source::Interrupted {
            match self.finish_interrupted() {
                Ok(committed) => {
                    done.finished = true;
                    done.warnings.extend(
                        committed
                            .pruning
                            .map(|why| format!("old generations were not removed: {why}")),
                    );
                }
                Err(e) => done
                    .failed
                    .push(format!("finishing the interrupted save failed: {e}")),
            }
        }
        done
    }

    /// The file's bytes (`None`: it does not exist). An I/O error is tried
    /// again, `READ_TRIES` times in all with a pause between (a sharing
    /// violation, a transient EIO), then it is the error (#32 P2).
    fn read_tried(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        let mut last = None;
        for attempt in 0..READ_TRIES {
            if attempt > 0 {
                self.files.pause();
            }
            match self.files.read(path) {
                Ok(bytes) => return Ok(Some(bytes)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| io::Error::other("no read was tried")))
    }

    fn quarantine(&self, current: &Path) -> io::Result<PathBuf> {
        for n in 1..=QUARANTINE_NAMES {
            let aside = self.dir.join(format!("{CURRENT}.damaged-{n}"));
            if !self.files.exists(&aside)? {
                self.files.rename(current, &aside)?;
                self.files.sync_dir(&self.dir)?;
                return Ok(aside);
            }
        }
        Err(io::Error::other(format!(
            "{QUARANTINE_NAMES} damaged copies are aside already"
        )))
    }

    fn finish_interrupted(&self) -> io::Result<Committed> {
        self.files.sync_file(&self.dir.join(TMP))?;
        self.commit_tmp()
    }

    /// Reads and decodes `path`. A file that does not decode goes to
    /// `rejected` with the reason; one that cannot be read too when
    /// `Tolerant`, and is the error when `Strict`.
    fn read_state(&self, path: &Path, pick: &mut Pick, reading: Reading) -> io::Result<Read> {
        let bytes = match self.read_tried(path) {
            Ok(Some(b)) => b,
            Ok(None) => return Ok(Read::Missing),
            Err(e) if reading == Reading::Strict => return Err(e),
            Err(e) => {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                pick.alarms.push(format!(
                    "{name} cannot be read ({e}): the state loaded may be older"
                ));
                pick.rejected.push((path.to_path_buf(), e.to_string()));
                return Ok(Read::Unreadable);
            }
        };
        Ok(match decode(&bytes) {
            Ok(persisted) => Read::Valid(persisted),
            Err(why) => {
                pick.rejected.push((path.to_path_buf(), why));
                Read::Damaged
            }
        })
    }
}

/// How `read_state` treats a file it cannot read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reading {
    /// The engine's boot: the file is `rejected`, the chain goes on.
    Tolerant,
    /// The seed: an error (it fails closed).
    Strict,
}

/// What the chain found at a state file (#32 P2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FileState {
    #[default]
    Missing,
    /// It could not be read: an I/O error (locked, no access, a failing
    /// disk).
    Unreadable,
    /// It was read but does not decode (format, schema, SHA-256, parse).
    Damaged,
    Valid,
}

/// One state file, read and decoded.
enum Read {
    Missing,
    Unreadable,
    Damaged,
    Valid(Persisted),
}

impl Read {
    fn state(&self) -> FileState {
        match self {
            Self::Missing => FileState::Missing,
            Self::Unreadable => FileState::Unreadable,
            Self::Damaged => FileState::Damaged,
            Self::Valid(_) => FileState::Valid,
        }
    }
}

/// What a pick gathers besides its choice.
#[derive(Debug, Default)]
struct Pick {
    rejected: Vec<(PathBuf, String)>,
    alarms: Vec<String>,
    current_json: FileState,
}

/// Whether an interrupted save (`save.tmp`) supersedes the state it
/// competes with (a valid `current.json`, or the newest valid generation
/// when `current.json` is missing or damaged): when its revision is at
/// least as high. `rev` is the core's own monotonic revision (one per
/// changing request, carried across restarts); no clock is consulted.
///
/// A tie goes to `save.tmp` (#32 review m2). Since D6 only `save` writes
/// it, so it is always a save that was cut off, never older than the file
/// beside it; after a fallback boot the core restarts at an older
/// generation's revision and edits can reach the lost file's revision
/// again, and at that tie the stale file must lose (recovery also moves a
/// damaged `current.json` aside, so it cannot come back). A lower revision
/// never wins: an import's fresh count (0) and an older engine's leftover
/// baseline never roll the saved state back (such a baseline at the same
/// revision holds that revision's state).
fn supersedes(interrupted: &Persisted, other: &Persisted) -> bool {
    interrupted.rev >= other.rev
}

/// A loaded state, reconciled against the topology.
fn settle(topo: &Topology, mut persisted: Persisted, source: Source, pick: Pick) -> Loaded {
    let (r, dropped) = reconcile(topo, &persisted.state);
    persisted.state = to_state(topo, &r);
    Loaded {
        persisted,
        source,
        rejected: pick.rejected,
        dropped,
        alarms: pick.alarms,
        current_json: pick.current_json,
    }
}

#[cfg(test)]
mod tests {
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
        fs::remove_file(s.dir().join("gen-0000000001.json")).unwrap();
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
            loaded.rejected.iter().any(|(path, why)| path == s.dir()
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
        assert_eq!(loaded.source, Source::Baseline);
        assert_eq!(loaded.persisted.rev, 4);
        assert_eq!(loaded.rejected.len(), 1);
        assert!(loaded.rejected[0].0.ends_with(CURRENT));
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
        let payload = r#"{"rev":7,"state":{"inputs":{"mic1":{"trim_db":-2.0,"colour":"red"}}},"future":{"x":1}}"#;
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
}
