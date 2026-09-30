//! Persistence (program spec §2.4, I10; design note §3.5): atomic,
//! checksummed state files in the state directory —
//!
//! - `current.json`: the latest save;
//! - `gen-<seq>.json`: the 20 previous saves (the newest has the highest seq);
//! - `baseline.json`: written at each import (and, from S6, at `live` entry);
//! - `save.new`: a save being written (never a load source);
//! - `save.tmp`: a complete save before its renames, always replaced whole
//!   (a crash before the second rename leaves the newest state only in it;
//!   `chain` decides when it is the live state, and the boot's recovery
//!   finishes that save);
//! - `baseline.tmp`: a baseline before its rename;
//! - `current.json.damaged-<n>`: a damaged `current.json` the boot moved
//!   aside, never read again;
//! - `engine.lock`: the engine holding the directory (`Store::lock`).
//!
//! A file is `{"format", "schema", "sha256", "payload"}`; the SHA-256 covers the
//! payload's raw bytes, so a re-serialisation never matters. Readers ignore
//! unknown fields and default missing ones (additive schemas). The load chain
//! (`chain`, which also holds the save protocol, the Missing / Unreadable /
//! Damaged / Valid states and the boot recovery) is current or `save.tmp`
//! → generations (newest first) → baseline → defaults with every mix
//! muted. Files of an older schema (1: the REAPER-shaped graph before #20)
//! are refused. Every file operation goes through `files::Files`, so tests
//! fail any single step (`fault_tests`).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iem_engine_proto::{MixId, MixState, SCHEMA};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};

use crate::core::{defaults_muted, reconcile, to_state};
use crate::topology::Topology;

mod chain;
#[cfg(test)]
mod fault_tests;
mod files;

use files::{Files, OsFiles};

pub use chain::{FileState, Recovery};

pub const FORMAT: &str = "iemmixer-state";
pub const GENERATIONS: usize = 20;
const CURRENT: &str = "current.json";
const BASELINE: &str = "baseline.json";
const TMP: &str = "save.tmp";
const NEW: &str = "save.new";
const LOCK: &str = "engine.lock";
const BASELINE_TMP: &str = "baseline.tmp";

/// What a state file carries.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Persisted {
    pub rev: u64,
    pub topology_hash: String,
    pub saved_unix_ms: u64,
    pub state: MixState,
    /// X14 limiter-active samples per mix (Q4: kept until reset).
    pub counters: BTreeMap<MixId, u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Current,
    /// `save.tmp`: a save a crash cut off before its renames, the newest
    /// state there is (its revision at least that of `current.json`, or of
    /// the newest generation when `current.json` is missing or damaged; #32).
    /// The boot's recovery makes it `current.json`.
    Interrupted,
    Generation(u64),
    Baseline,
    Defaults,
}

impl Source {
    /// The state file this source names in the state directory; `None` for
    /// the muted defaults.
    pub fn file_name(self) -> Option<String> {
        match self {
            Self::Current => Some(CURRENT.to_owned()),
            Self::Interrupted => Some(TMP.to_owned()),
            Self::Generation(seq) => Some(generation_name(seq)),
            Self::Baseline => Some(BASELINE.to_owned()),
            Self::Defaults => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    /// Reconciled against the topology.
    pub persisted: Persisted,
    pub source: Source,
    /// Files that exist but could not be used, and why.
    pub rejected: Vec<(PathBuf, String)>,
    /// Ids the topology no longer has.
    pub dropped: Vec<String>,
    /// Why the state loaded may be older than one the directory holds
    /// (the engine raises each as an alarm, #32).
    pub alarms: Vec<String>,
    /// What the chain found at `current.json` (the boot's recovery acts on
    /// it, #32).
    pub current_json: FileState,
}

#[derive(Serialize)]
struct FileOut<'a> {
    format: &'a str,
    schema: u32,
    sha256: String,
    payload: &'a RawValue,
}

#[derive(Deserialize)]
struct FileIn<'a> {
    format: String,
    schema: u32,
    sha256: String,
    #[serde(borrow)]
    payload: &'a RawValue,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn digest(text: &str) -> String {
    hex(&Sha256::digest(text.as_bytes()))
}

pub fn encode(p: &Persisted) -> io::Result<Vec<u8>> {
    let payload = RawValue::from_string(serde_json::to_string(p)?)?;
    let file = FileOut {
        format: FORMAT,
        schema: SCHEMA,
        sha256: digest(payload.get()),
        payload: &payload,
    };
    Ok(serde_json::to_vec(&file)?)
}

pub fn decode(bytes: &[u8]) -> Result<Persisted, String> {
    let file: FileIn<'_> =
        serde_json::from_slice(bytes).map_err(|e| format!("not a state file: {e}"))?;
    if file.format != FORMAT {
        return Err(format!(
            "format {:?} is not {FORMAT}",
            file.format.chars().take(40).collect::<String>()
        ));
    }
    if file.schema < SCHEMA {
        return Err(format!(
            "schema {} predates the engine model (schema {SCHEMA})",
            file.schema
        ));
    }
    if digest(file.payload.get()) != file.sha256 {
        return Err("checksum mismatch".into());
    }
    serde_json::from_str(file.payload.get()).map_err(|e| format!("payload: {e}"))
}

fn generation_name(seq: u64) -> String {
    format!("gen-{seq:010}.json")
}

fn generation_seq(name: &str) -> Option<u64> {
    name.strip_prefix("gen-")?
        .strip_suffix(".json")?
        .parse()
        .ok()
}

/// The generation files among a directory's entries (name, path), oldest
/// first. An entry that fails to read is that error, never "no generation"
/// (#32 m3: the seed would read it as no state).
fn generation_entries(
    entries: impl Iterator<Item = io::Result<(OsString, PathBuf)>>,
) -> io::Result<Vec<(u64, PathBuf)>> {
    let mut gens = Vec::new();
    for entry in entries {
        let (name, path) = entry?;
        if let Some(seq) = name.to_str().and_then(generation_seq) {
            gens.push((seq, path));
        }
    }
    gens.sort();
    Ok(gens)
}

/// The state directory taken by one engine (#32 P5): released when dropped
/// (or when the process ends).
#[derive(Debug)]
pub struct StateLock {
    _file: fs::File,
}

/// A save that reached `current.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committed {
    /// The generation the previous `current.json` became (0: there was none).
    pub generation: u64,
    /// Removing generations beyond [`GENERATIONS`] failed, why: the save
    /// itself stands.
    pub pruning: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
    /// Every file operation goes through here (#32 P9).
    files: Arc<dyn Files>,
}

impl Store {
    pub fn open(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            files: Arc::new(OsFiles),
        })
    }

    /// A store whose file operations a test controls (#32 P9).
    #[cfg(test)]
    pub(crate) fn with_files(dir: &Path, files: Arc<dyn Files>) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            files,
        })
    }

    /// Writes `bytes` to `path` and flushes it.
    fn write_synced(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.files.write(path, bytes)?;
        self.files.sync_file(path)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Takes the state directory for this engine (or `iem-migrate import`),
    /// before anything in it is read or written: the boot recovery moves
    /// files, so a second engine, or an import while an engine runs, gets a
    /// `WouldBlock` error at once and stops before it touches a state file
    /// (#32 P5). The lock is an OS file lock on `engine.lock`, released
    /// with the lock or the process.
    pub fn lock(&self) -> io::Result<StateLock> {
        let file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(self.dir.join(LOCK))?;
        match file.try_lock() {
            Ok(()) => Ok(StateLock { _file: file }),
            Err(fs::TryLockError::WouldBlock) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "the state directory {} is in use by another engine",
                    self.dir.display()
                ),
            )),
            Err(fs::TryLockError::Error(e)) => Err(e),
        }
    }

    fn generation_path(&self, seq: u64) -> PathBuf {
        self.dir.join(generation_name(seq))
    }

    /// Generation files, oldest first.
    pub fn generations(&self) -> io::Result<Vec<(u64, PathBuf)>> {
        generation_entries(self.files.list(&self.dir)?.into_iter())
    }

    /// Saves atomically; the previous `current.json` becomes the newest
    /// generation (its seq in the result, 0: there was none). The
    /// state is written and flushed to `save.new`, which then replaces
    /// `save.tmp` in one rename: `save.tmp` may hold the only copy of the
    /// newest state (an interrupted save not yet finished), so it is never
    /// truncated or written in place, only ever the previous complete save
    /// or the new one (#32 P1).
    pub fn save(&self, p: &Persisted) -> io::Result<Committed> {
        let bytes = encode(p)?;
        let new = self.dir.join(NEW);
        self.write_synced(&new, &bytes)?;
        self.files.rename(&new, &self.dir.join(TMP))?;
        self.commit_tmp()
    }

    /// The renames that end a save: the previous `current.json` becomes the
    /// newest generation and `save.tmp` (synced) becomes `current.json`,
    /// then the directory is synced. Whether `current.json` exists must be
    /// known: an error there is the save's error, before `save.tmp` could
    /// replace it (#32 P7; `save.tmp` keeps the newest state). Pruning to
    /// [`GENERATIONS`] comes after the commit and apart: its failure is
    /// `Committed::pruning`, never the save's (#32 P6).
    fn commit_tmp(&self) -> io::Result<Committed> {
        let current = self.dir.join(CURRENT);
        let mut generation = 0;
        if self.files.exists(&current)? {
            generation = self.generations()?.last().map_or(0, |g| g.0) + 1;
            self.files
                .rename(&current, &self.generation_path(generation))?;
        }
        self.files.rename(&self.dir.join(TMP), &current)?;
        self.files.sync_dir(&self.dir)?;
        Ok(Committed {
            generation,
            pruning: self.prune().err().map(|e| e.to_string()),
        })
    }

    /// Removes the oldest generations beyond [`GENERATIONS`]. A removal
    /// lost in a crash only leaves a file the next save removes.
    fn prune(&self) -> io::Result<()> {
        let gens = self.generations()?;
        let excess = gens.len().saturating_sub(GENERATIONS);
        for (_, path) in gens.iter().take(excess) {
            self.files.remove(path)?;
        }
        Ok(())
    }

    /// Writes `baseline.json` through its own `baseline.tmp`, never
    /// `save.tmp`, which may hold the only copy of an interrupted save (#32).
    pub fn save_baseline(&self, p: &Persisted) -> io::Result<()> {
        let tmp = self.dir.join(BASELINE_TMP);
        self.write_synced(&tmp, &encode(p)?)?;
        self.files.rename(&tmp, &self.dir.join(BASELINE))?;
        self.files.sync_dir(&self.dir)
    }
}

/// Saves 1 s after the last change, at most 5 s after the first (§2.4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SaveSchedule {
    first: Option<Instant>,
    last: Option<Instant>,
}

pub const QUIET: Duration = Duration::from_secs(1);
pub const MAX_DELAY: Duration = Duration::from_secs(5);

impl SaveSchedule {
    pub fn changed(&mut self, now: Instant) {
        self.first.get_or_insert(now);
        self.last = Some(now);
    }

    pub fn pending(&self) -> bool {
        self.first.is_some()
    }

    pub fn due(&self, now: Instant) -> bool {
        match (self.first, self.last) {
            (Some(first), Some(last)) => {
                now.saturating_duration_since(last) >= QUIET
                    || now.saturating_duration_since(first) >= MAX_DELAY
            }
            _ => false,
        }
    }

    pub fn saved(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_site;
    use iem_engine_proto::{InputId, InputState, Level, Mix};

    pub(super) fn sample(rev: u64) -> Persisted {
        let mut p = Persisted {
            rev,
            topology_hash: "abc".into(),
            saved_unix_ms: 1_790_000_000_000 + rev,
            ..Persisted::default()
        };
        p.state.inputs.insert(
            InputId::new("mic1"),
            InputState {
                trim_db: 0.1 + 0.2,
                ..InputState::default()
            },
        );
        let mut mix = Mix::default();
        mix.out.volume_db = -3.0 - rev as f64 / 7.0;
        mix.inputs.insert(
            InputId::new("mic1"),
            Level {
                gain_db: 12.041199826559248 - 12.0,
                pan: -1e-300,
                muted: false,
            },
        );
        p.state.mixes.insert(MixId::new("member1"), mix);
        p.counters.insert(MixId::new("member1"), 123_456_789 + rev);
        p
    }

    pub(super) fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state")).unwrap();
        (dir, store)
    }

    #[test]
    fn a_generation_entry_that_cannot_be_read_is_an_error() {
        // #32 m3: a failed directory entry was dropped, so a generation the
        // seed could not see read as "no state".
        let entry = |name: &str, path: &str| Ok((OsString::from(name), PathBuf::from(path)));
        let failing = vec![
            entry("gen-0000000002.json", "a"),
            Err(io::Error::other("entry unreadable")),
            entry("gen-0000000001.json", "b"),
        ];
        let e = generation_entries(failing.into_iter()).unwrap_err();
        assert_eq!(e.to_string(), "entry unreadable");
        // Readable entries: the generations, oldest first, nothing else.
        let fine = vec![
            entry("gen-0000000002.json", "a"),
            entry("current.json", "c"),
            entry("gen-0000000001.json", "b"),
        ];
        assert_eq!(
            generation_entries(fine.into_iter()).unwrap(),
            vec![(1, PathBuf::from("b")), (2, PathBuf::from("a"))]
        );
    }

    #[test]
    fn encode_decode_is_bit_exact_and_tamper_evident() {
        let p = sample(3);
        let bytes = encode(&p).unwrap();
        assert_eq!(decode(&bytes).unwrap(), p);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with(r#"{"format":"iemmixer-state","schema":2,"sha256":""#));
        let tampered = text.replace("123456792", "123456793");
        assert_ne!(tampered, text);
        assert_eq!(
            decode(tampered.as_bytes()).unwrap_err(),
            "checksum mismatch"
        );
        let foreign = text.replace("iemmixer-state", "other");
        assert!(decode(foreign.as_bytes()).unwrap_err().contains("other"));
        // A file of the REAPER-shaped model (schema 1) is refused, even intact.
        let old = text.replacen("\"schema\":2", "\"schema\":1", 1);
        assert_eq!(
            decode(old.as_bytes()).unwrap_err(),
            "schema 1 predates the engine model (schema 2)"
        );
        assert!(
            decode(b"{\"format\":")
                .unwrap_err()
                .starts_with("not a state file")
        );
    }

    #[test]
    fn each_source_names_its_file() {
        assert_eq!(Source::Current.file_name().as_deref(), Some(CURRENT));
        assert_eq!(Source::Interrupted.file_name().as_deref(), Some(TMP));
        assert_eq!(
            Source::Generation(7).file_name().as_deref(),
            Some("gen-0000000007.json")
        );
        assert_eq!(Source::Baseline.file_name().as_deref(), Some(BASELINE));
        assert_eq!(Source::Defaults.file_name(), None);
    }

    #[test]
    fn a_baseline_write_leaves_an_interrupted_save_alone() {
        let (_d, s) = store();
        fs::write(s.dir().join(TMP), b"NEWEST").unwrap();
        s.save_baseline(&sample(3)).unwrap();
        assert_eq!(fs::read(s.dir().join(TMP)).unwrap(), b"NEWEST");
        let baseline = fs::read(s.dir().join(BASELINE)).unwrap();
        assert_eq!(decode(&baseline).unwrap().rev, 3);
    }

    #[test]
    fn save_then_load_is_lossless() {
        let (_d, s) = store();
        let g = test_site();
        let mut p = sample(1);
        p.state = to_state(&g, &reconcile(&g, &p.state).0);
        assert_eq!(s.save(&p).unwrap(), 0);
        let loaded = s.load(&g);
        assert_eq!(loaded.source, Source::Current);
        assert_eq!(loaded.persisted, p);
        assert!(loaded.rejected.is_empty());
        let i = loaded.persisted.state.inputs[&InputId::new("mic1")];
        assert_eq!(i.trim_db, 0.1 + 0.2);
        let l = loaded.persisted.state.mixes[&MixId::new("member1")].inputs[&InputId::new("mic1")];
        assert_eq!(l.pan, -1e-300);
        assert_eq!(l.gain_db, 12.041199826559248 - 12.0);
    }

    #[test]
    fn a_save_rotates_current_into_a_generation_and_keeps_20() {
        let (_d, s) = store();
        for rev in 1..=25 {
            assert_eq!(s.save(&sample(rev)).unwrap().generation, rev - 1);
        }
        let gens = s.generations().unwrap();
        assert_eq!(gens.len(), GENERATIONS);
        assert_eq!(gens.first().unwrap().0, 5);
        assert_eq!(gens.last().unwrap().0, 24);
        assert_eq!(
            decode(&fs::read(&gens.last().unwrap().1).unwrap())
                .unwrap()
                .rev,
            24
        );
        assert_eq!(
            decode(&fs::read(s.dir().join(CURRENT)).unwrap())
                .unwrap()
                .rev,
            25
        );
        assert!(!s.dir().join(TMP).exists());
    }

    #[test]
    fn schedule_waits_one_quiet_second_but_at_most_five() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut s = SaveSchedule::default();
        assert!(!s.pending());
        assert!(!s.due(at(10_000)));
        s.changed(at(0));
        assert!(s.pending());
        assert!(!s.due(at(999)));
        assert!(s.due(at(1000)));
        // Changes every 0.5 s postpone the save, but not beyond 5 s.
        for k in 1..=9 {
            s.changed(at(500 * k));
            assert!(!s.due(at(500 * k + 400)), "{k}");
        }
        s.changed(at(4900));
        assert!(!s.due(at(4999)));
        assert!(s.due(at(5000)));
        s.saved();
        assert!(!s.pending());
        assert!(!s.due(at(9000)));
        // A clock that went backwards never panics.
        s.changed(at(2000));
        assert!(!s.due(at(1000)));
    }
}
