//! The load chain (#32): which file holds the state (`current.json`, an
//! interrupted save in `save.tmp`, a generation, the baseline), what the
//! seed must keep (`Store::live_state`), and the boot's recovery that
//! normalizes the directory to what was loaded (`Store::recover`).
//!
//! **The save protocol.** `save` writes the new state to `save.new` and
//! flushes it, renames `save.new` over `save.tmp` (so `save.tmp` is only
//! ever a complete save, never written in place), then renames
//! `current.json` to the next generation (named with the revision the
//! marker shows for it), the marker `current.json.rev-<rev>` to the new
//! revision, and `save.tmp` to `current.json`, and syncs the directory;
//! pruning old generations comes after, apart. A crash before the last
//! rename leaves the newest state only in `save.tmp`.
//!
//! **Each file is Missing, Unreadable, Damaged or Valid.** Unreadable: an
//! I/O error (a lock, no access, a failing disk); one that may pass (a
//! sharing or lock violation, an interrupted or timed-out read) is tried
//! again, up to `READ_TRIES` reads with a pause between and
//! `READ_PAUSES` pauses per load in all, any other at once (#32
//! minor-5). The file is named in an alarm and the best Valid candidate
//! loads.
//! Damaged: read fine but it does not decode (format, schema, SHA-256,
//! parse). Valid carries its revision.
//!
//! **The pick.** `save.tmp` is the live state when it is Valid and its
//! revision is at least that of the file it competes with
//! ([`supersedes`]): a Valid `current.json`, or the newest Valid generation
//! when `current.json` is not Valid (a save.tmp that could not be compared
//! because the generations cannot be listed loads only with an alarm).
//! Otherwise a Valid `current.json`, then the newest Valid generation, then
//! the baseline, then muted defaults. A Valid `save.tmp` passed over for a
//! lower revision is only legitimate as a leftover: it is named in an
//! alarm (and the next save moves it aside).
//!
//! **Past an Unreadable live file** (`current.json`, or a generation newer
//! than the one loaded) the state loaded (a generation, `save.tmp`, the
//! baseline or the defaults) continues its revision `REV_JUMP` above the
//! highest revision it or any name in the directory shows, with an alarm
//! (#32 MAJOR-3, F3-r4 2): the file may hold any revision the last session
//! reached, recovery never moves it, and once it can be read again it must
//! not outrank the saves made since this boot. A floor only the contents
//! carry is unknowable exactly then, so every commit writes revisions into
//! names (a generation's carries the revision it holds, the marker's the
//! one `current.json` holds), which a listing shows whatever a file's
//! contents do. Names without a revision (an older engine's) show none:
//! the jump then counts from the state loaded, as before.
//!
//! **Recovery at boot**, before the engine writes (under `Store::lock`):
//! a Damaged `current.json` is moved aside to `current.json.damaged-<n>`
//! (never read again), and a boot on `save.tmp` finishes that save with
//! `save`'s own steps. Recovery never moves, renames or rotates an
//! Unreadable file, never truncates `save.tmp`, and never loads or keeps
//! `save.new`; a step that fails is reported and the engine runs on the
//! loaded state (`save.tmp` stays whole for the next save or boot).
//!
//! **A `save.tmp` the boot did not load** (Unreadable, Damaged, or older
//! than the state loaded) is not the store's to replace: the next save
//! moves it aside to `save.tmp.orphan-<n>` and flushes the directory
//! before `save.new` takes the name; if that fails, the save fails and
//! nothing is replaced (#32 MAJOR-1). The move is logged, and every error
//! of that save after it names the orphan. An orphan is kept for
//! inspection, named in an alarm, and never a load source: it holds state
//! the engine never ran on, and the session's saves since the boot are
//! what the band hears, so no revision may bring it back. A failure or a
//! crash right after the move aside leaves the newest pending state only
//! there, so every boot names in an alarm each orphan whose revision is
//! above the state loaded (#32 F3-r4 1).

use tracing::warn;

use super::*;

/// Names tried for a file moved aside (`current.json.damaged-<n>`,
/// `save.tmp.orphan-<n>`): 1 up to this.
const QUARANTINE_NAMES: u32 = 1000;

/// How far above the highest revision the state loaded or any name in the
/// directory shows a boot past an Unreadable live file continues (#32
/// MAJOR-3, F3-r4 2). The names show every committed revision; above them
/// only a save not yet committed (`save.tmp`) can lie, and the core's
/// revision grows by one per changing request: a million requests is far
/// beyond what one session reaches, so the session's saves outrank
/// whatever that file holds. Each such boot jumps again; the `u64`
/// revision cannot run out.
const REV_JUMP: u64 = 1_000_000;

/// Reads of a file that fail with an error that may pass, with a pause
/// between them, before it counts as unreadable (#32 P2).
const READ_TRIES: usize = 5;

/// Pauses between read tries in one load, all files together (#32
/// minor-5): 10 of `files::READ_PAUSE` (200 ms) are 2 s, so however many
/// files stay locked the engine listens well within the guard's READY_S
/// (10 s). A file read once they are spent still gets its one try.
const READ_PAUSES: usize = 10;

/// Windows' `ERROR_SHARING_VIOLATION` and `ERROR_LOCK_VIOLATION`: another
/// process holds the file a moment.
const SHARING_VIOLATION: i32 = 32;
const LOCK_VIOLATION: i32 = 33;

/// Whether a read that failed may succeed when tried again (#32 minor-5):
/// an interrupted, would-block or timed-out read, or on Windows a sharing
/// or lock violation. Anything else (a directory, no access, a path part
/// that is no directory) is Unreadable at once.
fn transient(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    ) || (cfg!(windows) && matches!(e.raw_os_error(), Some(SHARING_VIOLATION | LOCK_VIOLATION)))
}

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
        Ok(self.live()?.map(|(source, _)| source))
    }

    /// The name of the file `live_state` names, as it is on disk (a
    /// generation's name carries its revision, #32 F3-r4 2).
    pub fn live_file(&self) -> io::Result<Option<String>> {
        Ok(self.live()?.map(|(_, path)| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        }))
    }

    /// The strict pick's source and file (`live_state`).
    fn live(&self) -> io::Result<Option<(Source, PathBuf)>> {
        Ok(self
            .pick_live(&mut Pick::default(), Reading::Strict)?
            .map(|(_, source, path)| (source, path)))
    }

    /// The load chain: the live state (`current.json`, `save.tmp` or the
    /// newest valid generation, see the module docs), else `baseline.json`,
    /// else defaults with every mix muted. A file must pass `decode`
    /// (format, schema, the payload's SHA-256, then the payload's parse) to
    /// be used; one that exists and does not, or cannot be read, is
    /// `rejected` with the reason, as is a `save.tmp` passed over for an
    /// older revision and a directory whose generations cannot be listed.
    /// Past an Unreadable `current.json` or newer generation the revision
    /// continues `REV_JUMP` above the highest revision the state loaded or
    /// any name shows (#32 MAJOR-3, F3-r4 2).
    pub fn load(&self, topo: &Topology) -> Loaded {
        let (mut loaded, passed_over) = self.load_chain(topo);
        // An orphan is compared with the state loaded's own revision, never
        // the jumped one (lane G4 review); one listing serves both.
        let own = loaded.persisted.rev;
        let listed = self.list();
        if !passed_over.is_empty() {
            continue_above(&mut loaded, &passed_over, &listed);
        }
        self.orphans_above(&mut loaded, own, &listed);
        loaded
    }

    /// Names in an alarm each orphan (`save.tmp.orphan-<n>`) that holds a
    /// revision above `own`, the state loaded's (#32 F3-r4 1). An orphan
    /// is never loaded, but a failure or a crash right after its move aside
    /// leaves the newest pending state only there. Each is read once,
    /// without the chain's pauses (the boot's bound stays); one that does
    /// not decode is no state, one that cannot be read and a listing that
    /// failed are named too.
    fn orphans_above(&self, loaded: &mut Loaded, own: u64, listed: &io::Result<Listing>) {
        let orphans = match listed {
            Ok(listed) => &listed.orphans,
            Err(e) => {
                loaded.alarms.push(format!(
                    "the state directory cannot be listed to look for a {TMP} \
                     moved aside above the state loaded ({e})"
                ));
                return;
            }
        };
        for (_, name, path) in orphans {
            let mut spent = READ_PAUSES;
            match self.read_tried(path, &mut spent) {
                Ok(Some(bytes)) => {
                    if let Ok(orphan) = decode(&bytes)
                        && orphan.rev > own
                    {
                        loaded.alarms.push(format!(
                            "{name} (revision {}) is above the state loaded (revision {own}): \
                             a {TMP} moved aside, kept but never loaded",
                            orphan.rev
                        ));
                    }
                }
                Ok(None) => {}
                Err(e) => loaded.alarms.push(format!(
                    "{name} cannot be read ({e}): a {TMP} moved aside, it may hold \
                     a state above the one loaded"
                )),
            }
        }
    }
}

/// Continues the loaded revision `REV_JUMP` above the highest one the state
/// loaded or any name in the directory shows (`listed`), past the live
/// files `passed_over` that could not be read (#32 MAJOR-3, F3-r4 2), with
/// an alarm. A listing that failed leaves the state loaded's own revision as
/// the base (named in an alarm too).
fn continue_above(loaded: &mut Loaded, passed_over: &[String], listed: &io::Result<Listing>) {
    let floor = match listed {
        Ok(listed) => listed.floor,
        Err(e) => {
            loaded.doubt(format!(
                "the revisions the state files' names show cannot be listed ({e})"
            ));
            None
        }
    };
    let rev = loaded
        .persisted
        .rev
        .max(floor.unwrap_or(0))
        .saturating_add(REV_JUMP);
    loaded.persisted.rev = rev;
    let (names, it) = match passed_over {
        [one] => (one.clone(), "it"),
        more => (more.join(" and "), "they"),
    };
    let shown = floor.map_or_else(String::new, |floor| {
        format!(" (the names show revision {floor} at most)")
    });
    loaded.doubt(format!(
        "{names} cannot be read, so the revision continues at {rev}, \
         above anything {it} can hold{shown}"
    ));
}

impl Store {
    /// The chain itself (see `load`), and the live files it passed over
    /// because they could not be read.
    fn load_chain(&self, topo: &Topology) -> (Loaded, Vec<String>) {
        let mut pick = Pick::default();
        let live = self
            .pick_live(&mut pick, Reading::Tolerant)
            .unwrap_or_else(|e| {
                pick.rejected.push((self.dir.clone(), e.to_string()));
                None
            });
        let passed_over = std::mem::take(&mut pick.passed_over);
        if let Some((persisted, source, _)) = live {
            return (settle(topo, persisted, source, pick), passed_over);
        }
        let baseline = self.dir.join(BASELINE);
        if let Ok(Read::Valid(persisted)) = self.read_state(&baseline, &mut pick, Reading::Tolerant)
        {
            return (settle(topo, persisted, Source::Baseline, pick), passed_over);
        }
        let loaded = Loaded {
            persisted: Persisted {
                topology_hash: topo.hash.clone(),
                state: defaults_muted(topo),
                ..Persisted::default()
            },
            source: Source::Defaults,
            rejected: pick.rejected,
            dropped: Vec::new(),
            alarms: pick.alarms,
            doubts: pick.doubts,
            current_json: pick.current_json,
            save_tmp: pick.save_tmp,
        };
        (loaded, passed_over)
    }

    /// The live state, its source and its file, shared by `load` (tolerant:
    /// a file it cannot read is `rejected`, and one that could hold live
    /// state goes to `Pick::passed_over`) and `live_state` (strict: that is
    /// an error), so the seed names exactly the file the engine loads.
    fn pick_live(
        &self,
        pick: &mut Pick,
        reading: Reading,
    ) -> io::Result<Option<(Persisted, Source, PathBuf)>> {
        let tmp_path = self.dir.join(TMP);
        let current_path = self.dir.join(CURRENT);
        let current = self.read_state(&current_path, pick, reading)?;
        pick.current_json = current.state();
        if pick.current_json == FileState::Unreadable {
            pick.passed_over.push(CURRENT.to_owned());
        }
        let tmp = self.read_state(&tmp_path, pick, reading)?;
        pick.save_tmp = tmp.state();
        if let Read::Valid(current) = current {
            return Ok(Some(match tmp {
                Read::Valid(tmp) if supersedes(&tmp, &current) => {
                    (tmp, Source::Interrupted, tmp_path)
                }
                Read::Valid(tmp) => {
                    pick.rejected.push((
                        tmp_path,
                        format!(
                            "revision {} is not newer than current.json's {}",
                            tmp.rev, current.rev
                        ),
                    ));
                    pick.alarms.push(format!(
                        "{TMP} (revision {}) is older than {CURRENT}'s {} and is not loaded",
                        tmp.rev, current.rev
                    ));
                    (current, Source::Current, current_path)
                }
                Read::Missing | Read::Unreadable | Read::Damaged => {
                    (current, Source::Current, current_path)
                }
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
            match self.read_state(&path, pick, reading)? {
                Read::Valid(generation) => {
                    newest = Some((generation, seq, path));
                    break;
                }
                // #32 F3-r4 2: newer than the one that loads, it may hold
                // any revision its name shows.
                Read::Unreadable => pick.passed_over.push(
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                ),
                Read::Missing | Read::Damaged => {}
            }
        }
        Ok(match (tmp, newest) {
            (Read::Valid(tmp), Some((generation, seq, path))) if !supersedes(&tmp, &generation) => {
                pick.rejected.push((
                    tmp_path,
                    format!(
                        "revision {} is not newer than generation {seq}'s {}",
                        tmp.rev, generation.rev
                    ),
                ));
                pick.alarms.push(format!(
                    "{TMP} (revision {}) is older than generation {seq}'s {} and is not loaded",
                    tmp.rev, generation.rev
                ));
                Some((generation, Source::Generation(seq), path))
            }
            (Read::Valid(tmp), _) => {
                // #32 P8: compared with nothing when the listing failed.
                if let Some(e) = unlisted {
                    pick.doubt(format!(
                        "save.tmp is loaded without comparing it with the \
                         generations, which cannot be listed ({e})"
                    ));
                }
                Some((tmp, Source::Interrupted, tmp_path))
            }
            (_, Some((generation, seq, path))) => Some((generation, Source::Generation(seq), path)),
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
    /// (#32 review). A Damaged `current.json` is moved aside to the first
    /// free `current.json.damaged-<n>`, a name the load chain never reads,
    /// so it cannot come back as a stale state later; an Unreadable one is
    /// never touched (an interrupted save then stays in `save.tmp`, whole,
    /// with a warning). A boot on `save.tmp` then finishes that save the way
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
        // #32 MAJOR-1: save.tmp is this store's to replace only when it is
        // the state loaded, or there is none.
        self.tmp_own.store(
            loaded.save_tmp == FileState::Missing || loaded.source == Source::Interrupted,
            Ordering::SeqCst,
        );
        let current = self.dir.join(CURRENT);
        if loaded.current_json == FileState::Damaged {
            match self.move_aside(&current, "damaged") {
                Ok((aside, synced)) => {
                    // #32 minor-6: moved, only the sync after it failed.
                    if let Err(e) = synced {
                        done.failed.push(format!(
                            "the damaged {CURRENT} was moved aside to {}, but the \
                             directory sync failed: {e}",
                            aside.display()
                        ));
                    }
                    done.quarantined = Some(aside);
                }
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
            match self.finish_interrupted(loaded.persisted.rev) {
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

    /// The file's bytes (`None`: it does not exist). An error that may pass
    /// ([`transient`]) is tried again, up to `READ_TRIES` reads with a
    /// pause between, while the load's `paused` stays below `READ_PAUSES`;
    /// then, or for any other error, it is the error (#32 P2, minor-5).
    fn read_tried(&self, path: &Path, paused: &mut usize) -> io::Result<Option<Vec<u8>>> {
        let mut tries = 1;
        loop {
            match self.files.read(path) {
                Ok(bytes) => return Ok(Some(bytes)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(e) if transient(&e) && tries < READ_TRIES && *paused < READ_PAUSES => {
                    tries += 1;
                    *paused += 1;
                    self.files.pause();
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Moves a `save.tmp` that is not this store's own aside to the first
    /// free `save.tmp.orphan-<n>` and flushes the directory, before
    /// `save.new` takes its name; either failing fails the save with
    /// nothing replaced (#32 MAJOR-1). The move is logged when it happens
    /// (#32 F3-r4 1). `None`: nothing to move.
    pub(super) fn orphan_tmp(&self) -> io::Result<Option<PathBuf>> {
        let tmp = self.dir.join(TMP);
        if self.tmp_own.load(Ordering::SeqCst) || !self.files.exists(&tmp)? {
            return Ok(None);
        }
        let (aside, synced) = self.move_aside(&tmp, "orphan")?;
        warn!(
            "{TMP} was not this engine's to replace: moved aside to {}",
            aside.display()
        );
        synced.map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "{TMP} was moved aside to {}, but the directory sync failed: {e}",
                    aside.display()
                ),
            )
        })?;
        Ok(Some(aside))
    }

    /// Renames `path` to the first free `<name>.<tag>-<n>` beside it: where
    /// it went, and how the directory sync after it went. An error only
    /// when it was not moved.
    fn move_aside(&self, path: &Path, tag: &str) -> io::Result<(PathBuf, io::Result<()>)> {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        for n in 1..=QUARANTINE_NAMES {
            let aside = self.dir.join(format!("{name}.{tag}-{n}"));
            if !self.files.exists(&aside)? {
                self.files.rename(path, &aside)?;
                return Ok((aside, self.files.sync_dir(&self.dir)));
            }
        }
        Err(io::Error::other(format!(
            "{QUARANTINE_NAMES} copies of {name} are aside already"
        )))
    }

    /// `save`'s commit of the `save.tmp` the boot loaded at `rev` (at
    /// least what it holds: a jumped revision only raises the marker).
    fn finish_interrupted(&self, rev: u64) -> io::Result<Committed> {
        self.files.sync_file(&self.dir.join(TMP))?;
        self.commit_tmp(rev)
    }

    /// Reads and decodes `path`. A file that does not decode goes to
    /// `rejected` with the reason; one that cannot be read too when
    /// `Tolerant`, and is the error, naming the file, when `Strict`.
    fn read_state(&self, path: &Path, pick: &mut Pick, reading: Reading) -> io::Result<Read> {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let bytes = match self.read_tried(path, &mut pick.paused) {
            Ok(Some(b)) => b,
            Ok(None) => return Ok(Read::Missing),
            Err(e) if reading == Reading::Strict => {
                return Err(io::Error::new(
                    e.kind(),
                    format!("{name} cannot be read: {e}"),
                ));
            }
            Err(e) => {
                pick.doubt(format!(
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
    /// The alarms that leave the live state in doubt (`Loaded::doubts`).
    doubts: Vec<String>,
    current_json: FileState,
    save_tmp: FileState,
    /// Live files that could not be read (`current.json`, a generation
    /// newer than the one that loads): the load continues above them.
    passed_over: Vec<String>,
    /// Pauses between read tries so far (`READ_PAUSES` at most).
    paused: usize,
}

impl Pick {
    /// An alarm that leaves the live state in doubt (`Loaded::doubts`).
    fn doubt(&mut self, alarm: String) {
        self.doubts.push(alarm.clone());
        self.alarms.push(alarm);
    }
}

impl Loaded {
    /// An alarm that leaves the live state in doubt (`Loaded::doubts`).
    fn doubt(&mut self, alarm: String) {
        self.doubts.push(alarm.clone());
        self.alarms.push(alarm);
    }
}

/// Whether an interrupted save (`save.tmp`) supersedes the state it
/// competes with (a valid `current.json`, or the newest valid generation
/// when `current.json` is missing or damaged): when its revision is at
/// least as high. `rev` is the core's own monotonic revision (one per
/// changing request, carried across restarts); no clock is consulted.
///
/// A tie goes to `save.tmp` (#32 review m2). Since D6 only `save` writes
/// it, so it is always a save that was cut off, never older than the file
/// beside it. After a fallback boot the core restarts at an older state's
/// revision, and the file it fell back from must never outrank the
/// session's saves: a Damaged `current.json` is moved aside by the
/// recovery (it cannot come back), and past an Unreadable one or newer
/// generation (which recovery never moves) the load continues the
/// revision `REV_JUMP` above the highest the state loaded or any name
/// shows (#32 MAJOR-3, F3-r4 2), so the session's saves outrank the file
/// once it can be read again. A lower revision never wins: an
/// import's fresh count (0) and an older engine's leftover baseline never
/// roll the saved state back (such a baseline at the same revision holds
/// that revision's state); such a `save.tmp` is named in an alarm.
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
        doubts: pick.doubts,
        current_json: pick.current_json,
        save_tmp: pick.save_tmp,
    }
}

#[cfg(test)]
mod tests;
