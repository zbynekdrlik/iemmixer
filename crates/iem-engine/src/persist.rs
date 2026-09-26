//! Persistence (program spec §2.4, I10; design note §3.5): atomic,
//! checksummed state files in the state directory —
//!
//! - `current.json`: the latest save;
//! - `gen-<seq>.json`: the 20 previous saves (the newest has the highest seq);
//! - `baseline.json`: written at each import (and, from S6, at `live` entry).
//!
//! A file is `{"format", "schema", "sha256", "payload"}`; the SHA-256 covers the
//! payload's raw bytes, so a re-serialisation never matters. Readers ignore
//! unknown fields and default missing ones (additive schemas). The load chain
//! is current → generations (newest first) → baseline → defaults with every
//! TX bus muted.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iem_engine_proto::{BusId, MixState, SCHEMA};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};

use crate::core::{defaults_muted, reconcile, to_mix};
use crate::graph::Graph;

pub const FORMAT: &str = "iemmixer-state";
pub const GENERATIONS: usize = 20;
const CURRENT: &str = "current.json";
const BASELINE: &str = "baseline.json";
const TMP: &str = "save.tmp";

/// What a state file carries.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Persisted {
    pub rev: u64,
    pub topology_hash: String,
    pub saved_unix_ms: u64,
    pub state: MixState,
    /// X14 limiter-active samples per bus (Q4: kept until reset).
    pub counters: BTreeMap<BusId, u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Current,
    Generation(u64),
    Baseline,
    Defaults,
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
    #[allow(dead_code)]
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
    if digest(file.payload.get()) != file.sha256 {
        return Err("checksum mismatch".into());
    }
    serde_json::from_str(file.payload.get()).map_err(|e| format!("payload: {e}"))
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

/// Windows has no directory handle to flush this way; the renames are
/// `MoveFileExW` with replace (S6 may switch to `ReplaceFileW`).
#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

fn generation_seq(name: &str) -> Option<u64> {
    name.strip_prefix("gen-")?
        .strip_suffix(".json")?
        .parse()
        .ok()
}

#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn open(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        Ok(Self {
            dir: dir.to_path_buf(),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn generation_path(&self, seq: u64) -> PathBuf {
        self.dir.join(format!("gen-{seq:010}.json"))
    }

    /// Generation files, oldest first.
    pub fn generations(&self) -> io::Result<Vec<(u64, PathBuf)>> {
        let mut gens: Vec<(u64, PathBuf)> = fs::read_dir(&self.dir)?
            .filter_map(Result::ok)
            .filter_map(|e| {
                let seq = generation_seq(e.file_name().to_str()?)?;
                Some((seq, e.path()))
            })
            .collect();
        gens.sort();
        Ok(gens)
    }

    /// Saves atomically; the previous `current.json` becomes the newest
    /// generation. Returns that generation's seq (0: there was none).
    pub fn save(&self, p: &Persisted) -> io::Result<u64> {
        let bytes = encode(p)?;
        let tmp = self.dir.join(TMP);
        write_synced(&tmp, &bytes)?;
        let current = self.dir.join(CURRENT);
        let mut seq = 0;
        if current.exists() {
            seq = self.generations()?.last().map_or(0, |g| g.0) + 1;
            fs::rename(&current, self.generation_path(seq))?;
        }
        fs::rename(&tmp, &current)?;
        let gens = self.generations()?;
        let excess = gens.len().saturating_sub(GENERATIONS);
        for (_, path) in gens.iter().take(excess) {
            fs::remove_file(path)?;
        }
        sync_dir(&self.dir)?;
        Ok(seq)
    }

    pub fn save_baseline(&self, p: &Persisted) -> io::Result<()> {
        let tmp = self.dir.join(TMP);
        write_synced(&tmp, &encode(p)?)?;
        fs::rename(&tmp, self.dir.join(BASELINE))?;
        sync_dir(&self.dir)
    }

    /// The load chain.
    pub fn load(&self, graph: &Graph) -> Loaded {
        let mut candidates = vec![(self.dir.join(CURRENT), Source::Current)];
        if let Ok(gens) = self.generations() {
            candidates.extend(
                gens.into_iter()
                    .rev()
                    .map(|(seq, path)| (path, Source::Generation(seq))),
            );
        }
        candidates.push((self.dir.join(BASELINE), Source::Baseline));
        let mut rejected = Vec::new();
        for (path, source) in candidates {
            let bytes = match fs::read(&path) {
                Ok(b) => b,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => {
                    rejected.push((path, e.to_string()));
                    continue;
                }
            };
            match decode(&bytes) {
                Ok(mut persisted) => {
                    let (r, dropped) = reconcile(graph, &persisted.state);
                    persisted.state = to_mix(graph, &r);
                    return Loaded {
                        persisted,
                        source,
                        rejected,
                        dropped,
                    };
                }
                Err(why) => rejected.push((path, why)),
            }
        }
        Loaded {
            persisted: Persisted {
                topology_hash: graph.hash.clone(),
                state: defaults_muted(graph),
                ..Persisted::default()
            },
            source: Source::Defaults,
            rejected,
            dropped: Vec::new(),
        }
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
    use iem_engine_proto::{BusKind, BusState, InputId, InputState};

    fn sample(rev: u64) -> Persisted {
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
                pan: -1e-300,
                fader_db: 12.041199826559248,
                ..InputState::default()
            },
        );
        p.state.buses.insert(
            BusId::new("member1"),
            BusState {
                fader_db: -3.0 - rev as f64 / 7.0,
                ..BusState::default()
            },
        );
        p.counters.insert(BusId::new("member1"), 123_456_789 + rev);
        p
    }

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state")).unwrap();
        (dir, store)
    }

    #[test]
    fn encode_decode_is_bit_exact_and_tamper_evident() {
        let p = sample(3);
        let bytes = encode(&p).unwrap();
        assert_eq!(decode(&bytes).unwrap(), p);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with(r#"{"format":"iemmixer-state","schema":1,"sha256":""#));
        let tampered = text.replace("123456792", "123456793");
        assert_ne!(tampered, text);
        assert_eq!(
            decode(tampered.as_bytes()).unwrap_err(),
            "checksum mismatch"
        );
        let foreign = text.replace("iemmixer-state", "other");
        assert!(decode(foreign.as_bytes()).unwrap_err().contains("other"));
        assert!(
            decode(b"{\"format\":")
                .unwrap_err()
                .starts_with("not a state file")
        );
    }

    #[test]
    fn save_then_load_is_lossless() {
        let (_d, s) = store();
        let g = test_site();
        let mut p = sample(1);
        p.state = to_mix(&g, &reconcile(&g, &p.state).0);
        assert_eq!(s.save(&p).unwrap(), 0);
        let loaded = s.load(&g);
        assert_eq!(loaded.source, Source::Current);
        assert_eq!(loaded.persisted, p);
        assert!(loaded.rejected.is_empty());
        let i = loaded.persisted.state.inputs[&InputId::new("mic1")];
        assert_eq!(i.trim_db, 0.1 + 0.2);
        assert_eq!(i.pan, -1e-300);
    }

    #[test]
    fn a_save_rotates_current_into_a_generation_and_keeps_20() {
        let (_d, s) = store();
        for rev in 1..=25 {
            assert_eq!(s.save(&sample(rev)).unwrap(), rev - 1);
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

    fn corrupt(path: &Path) {
        let mut text = fs::read_to_string(path).unwrap();
        let at = text.find("\"rev\":").unwrap() + 6;
        text.insert(at, '9');
        fs::write(path, text).unwrap();
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
        // A stray temp file never counts.
        fs::write(s.dir().join(TMP), b"garbage").unwrap();
        let loaded = s.load(&test_site());
        assert_eq!(loaded.source, Source::Generation(1));
        assert_eq!(loaded.persisted.rev, 1);
        assert_eq!(loaded.rejected.len(), 2);
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
        for n in &g.buses {
            let muted = loaded.persisted.state.buses[&n.id].muted;
            assert_eq!(muted, n.kind != BusKind::Stems, "{}", n.id);
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
        assert_eq!(loaded.persisted.state.sends.len(), 268);
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
