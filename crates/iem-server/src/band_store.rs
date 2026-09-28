//! Presets, history and pins/hides in the band schema-3 files (F8, F13, F14;
//! S4 design note §3.4, S5 design note §6): `presets/<member>.json`,
//! `snapshots/<member>.json`, `customizations/<member>.json` in the config
//! directory. A file of another format or schema is an error and is never
//! overwritten. Archived entries (a renamed member's history, D8) are
//! loadable but never changed.
//!
//! Also the two pure halves of presets and snapshots: `capture` (the page
//! mix from the mirror) and `ramp` (applying one as a 50 ms ramp).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use iem_core::band::{
    CUSTOMIZATION_FORMAT, CustomizationFile, MAX_PRESETS, MAX_SNAPSHOTS, MixSend, PRESETS_FORMAT,
    Preset, PresetFile, SCHEMA, SNAPSHOTS_FORMAT, Snapshot, SnapshotFile,
};
use iem_engine_proto::{Cmd, DB_OFF, Eq, GroupId, InputId, MixId, Source, db_to_lin};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::engine::mirror::Mirror;
use crate::site_view::{Page, SiteView};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("{0}")]
    Io(String),
    #[error("{0} is not a band schema-3 file: {1}")]
    Corrupt(String, String),
    #[error("maximum {MAX_PRESETS} presets reached")]
    Full,
    #[error("archived entries are read-only")]
    Archived,
    #[error("member id: {0}")]
    BadMember(String),
}

#[derive(Debug)]
pub struct BandStore {
    dir: PathBuf,
    lock: Mutex<()>,
}

/// Header fields every band file carries.
trait BandFile: Default + Serialize + DeserializeOwned {
    const FORMAT: &'static str;
    fn header(&self) -> (&str, u32);
    fn new_for(member: &str) -> Self;
}

impl BandFile for PresetFile {
    const FORMAT: &'static str = PRESETS_FORMAT;
    fn header(&self) -> (&str, u32) {
        (&self.format, self.schema)
    }
    fn new_for(member: &str) -> Self {
        PresetFile::new(member, Vec::new())
    }
}

impl BandFile for SnapshotFile {
    const FORMAT: &'static str = SNAPSHOTS_FORMAT;
    fn header(&self) -> (&str, u32) {
        (&self.format, self.schema)
    }
    fn new_for(member: &str) -> Self {
        SnapshotFile::new(member, Vec::new())
    }
}

impl BandFile for CustomizationFile {
    const FORMAT: &'static str = CUSTOMIZATION_FORMAT;
    fn header(&self) -> (&str, u32) {
        (&self.format, self.schema)
    }
    fn new_for(member: &str) -> Self {
        CustomizationFile::new(member, Vec::new(), Vec::new())
    }
}

/// The UTC day ("YYYY-MM-DD") of a Unix time.
pub fn utc_day(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

impl BandStore {
    pub fn new(config_dir: &Path) -> Self {
        Self {
            dir: config_dir.to_path_buf(),
            lock: Mutex::new(()),
        }
    }

    fn path(&self, kind: &str, member: &str) -> Result<PathBuf, StoreError> {
        iem_core::config::validate_member_id(member).map_err(StoreError::BadMember)?;
        Ok(self.dir.join(kind).join(format!("{member}.json")))
    }

    fn read<F: BandFile>(&self, kind: &str, member: &str) -> Result<F, StoreError> {
        let path = self.path(kind, member)?;
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(F::new_for(member)),
            Err(e) => return Err(StoreError::Io(format!("{}: {e}", path.display()))),
        };
        let file: F = serde_json::from_str(&text)
            .map_err(|e| StoreError::Corrupt(path.display().to_string(), e.to_string()))?;
        let (format, schema) = file.header();
        if format != F::FORMAT || schema != SCHEMA {
            return Err(StoreError::Corrupt(
                path.display().to_string(),
                format!("format {format:?}, schema {schema}"),
            ));
        }
        Ok(file)
    }

    fn write<F: BandFile>(&self, kind: &str, member: &str, file: &F) -> Result<(), StoreError> {
        let path = self.path(kind, member)?;
        let io = |e: std::io::Error| StoreError::Io(format!("{}: {e}", path.display()));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        let json = serde_json::to_string_pretty(file).map_err(|e| StoreError::Io(e.to_string()))?;
        crate::atomic_write(&path, &json).map_err(io)
    }

    fn guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    // ---- presets ----

    /// Presets, newest update first.
    pub fn presets(&self, member: &str) -> Result<Vec<Preset>, StoreError> {
        let mut list = self.read::<PresetFile>("presets", member)?.presets;
        list.sort_by_key(|p| std::cmp::Reverse(p.updated_at));
        Ok(list)
    }

    pub fn preset(&self, member: &str, name: &str) -> Result<Option<Preset>, StoreError> {
        Ok(self.presets(member)?.into_iter().find(|p| p.name == name))
    }

    /// Saves (or overwrites) `name` with this content at `now`.
    pub fn save_preset(
        &self,
        member: &str,
        name: &str,
        content: Captured,
        now: i64,
    ) -> Result<Preset, StoreError> {
        let _g = self.guard();
        let mut file = self.read::<PresetFile>("presets", member)?;
        let existing = file.presets.iter().position(|p| p.name == name);
        let created_at = match existing {
            Some(i) => {
                let p = &file.presets[i];
                if p.archived {
                    return Err(StoreError::Archived);
                }
                p.created_at
            }
            None if file.presets.len() >= MAX_PRESETS => return Err(StoreError::Full),
            None => now,
        };
        let preset = Preset {
            name: name.to_string(),
            created_at,
            updated_at: now,
            sends: content.sends,
            groups: content.groups,
            input_eq: content.input_eq,
            archived: false,
            legacy_member: None,
        };
        match existing {
            Some(i) => file.presets[i] = preset.clone(),
            None => file.presets.push(preset.clone()),
        }
        self.write("presets", member, &file)?;
        Ok(preset)
    }

    pub fn delete_preset(&self, member: &str, name: &str) -> Result<bool, StoreError> {
        let _g = self.guard();
        let mut file = self.read::<PresetFile>("presets", member)?;
        let Some(i) = file.presets.iter().position(|p| p.name == name) else {
            return Ok(false);
        };
        if file.presets[i].archived {
            return Err(StoreError::Archived);
        }
        file.presets.remove(i);
        self.write("presets", member, &file)?;
        Ok(true)
    }

    // ---- snapshots ----

    /// History, newest first.
    pub fn snapshots(&self, member: &str) -> Result<Vec<Snapshot>, StoreError> {
        let mut list = self.read::<SnapshotFile>("snapshots", member)?.snapshots;
        list.sort_by_key(|s| std::cmp::Reverse(s.timestamp));
        Ok(list)
    }

    pub fn snapshot(&self, member: &str, ts: i64) -> Result<Option<Snapshot>, StoreError> {
        Ok(self
            .snapshots(member)?
            .into_iter()
            .find(|s| s.timestamp == ts))
    }

    /// Adds a snapshot and prunes the oldest unpinned beyond 50.
    pub fn add_snapshot(&self, member: &str, snapshot: Snapshot) -> Result<(), StoreError> {
        let _g = self.guard();
        let mut file = self.read::<SnapshotFile>("snapshots", member)?;
        file.snapshots.push(snapshot);
        prune(&mut file.snapshots);
        self.write("snapshots", member, &file)
    }

    pub fn delete_snapshot(&self, member: &str, ts: i64) -> Result<bool, StoreError> {
        self.edit_snapshot(member, ts, |_, list, i| {
            list.remove(i);
        })
    }

    /// Pins (with a label) or unpins a snapshot.
    pub fn pin_snapshot(
        &self,
        member: &str,
        ts: i64,
        pinned: bool,
        label: Option<String>,
    ) -> Result<bool, StoreError> {
        self.edit_snapshot(member, ts, |_, list, i| {
            let s = &mut list[i];
            s.pinned = pinned;
            if let Some(l) = label {
                s.label = l;
            }
        })
    }

    fn edit_snapshot(
        &self,
        member: &str,
        ts: i64,
        f: impl FnOnce(&str, &mut Vec<Snapshot>, usize),
    ) -> Result<bool, StoreError> {
        let _g = self.guard();
        let mut file = self.read::<SnapshotFile>("snapshots", member)?;
        let Some(i) = file.snapshots.iter().position(|s| s.timestamp == ts) else {
            return Ok(false);
        };
        if file.snapshots[i].archived {
            return Err(StoreError::Archived);
        }
        f(member, &mut file.snapshots, i);
        self.write("snapshots", member, &file)?;
        Ok(true)
    }

    /// Whether an automatic snapshot exists for the UTC `day`.
    pub fn has_auto_snapshot_on(&self, member: &str, day: &str) -> Result<bool, StoreError> {
        Ok(self
            .snapshots(member)?
            .iter()
            .any(|s| s.label == AUTO_LABEL && utc_day(s.timestamp) == day))
    }

    // ---- customizations ----

    pub fn customization(&self, member: &str) -> Result<CustomizationFile, StoreError> {
        self.read("customizations", member)
    }

    pub fn save_customization(
        &self,
        member: &str,
        pinned: Vec<Source>,
        hidden: Vec<Source>,
    ) -> Result<(), StoreError> {
        let _g = self.guard();
        self.write(
            "customizations",
            member,
            &CustomizationFile::new(member, pinned, hidden),
        )
    }
}

/// Label of the daily automatic snapshot (F14).
pub const AUTO_LABEL: &str = "auto";

/// Oldest unpinned first out until at most 50 remain.
fn prune(list: &mut Vec<Snapshot>) {
    list.sort_by_key(|s| s.timestamp);
    while list.len() > MAX_SNAPSHOTS {
        match list.iter().position(|s| !s.pinned) {
            Some(i) => {
                list.remove(i);
            }
            None => break,
        }
    }
}

/// What a preset or snapshot holds of a page's mix.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Captured {
    pub sends: Vec<MixSend>,
    pub groups: BTreeMap<GroupId, f64>,
    /// Input EQ of the member's own inputs: metadata, never applied (Q2).
    pub input_eq: BTreeMap<InputId, Eq>,
}

/// The page mix as a preset or snapshot: every level (inputs, then heard
/// mixes), the stems strip's fader and the EQ of the member's inputs.
pub fn capture(view: &SiteView, mirror: &Mirror, page: &Page) -> Captured {
    let heard = view
        .mix(&page.mix)
        .map(|m| m.hears.clone())
        .unwrap_or_default();
    let sources = view
        .inputs
        .iter()
        .map(|i| Source::Input(i.id.clone()))
        .chain(heard.into_iter().map(Source::Mix));
    let sends = sources
        .map(|src| {
            let l = mirror.level(&page.mix, &src);
            MixSend {
                src,
                gain_db: l.gain_db,
                pan: l.pan,
                muted: l.muted,
            }
        })
        .collect();
    let groups = view
        .group
        .iter()
        .map(|g| (g.clone(), mirror.group(&page.mix, g).gain_db))
        .collect();
    let input_eq = view
        .inputs
        .iter()
        .filter(|i| page.member.is_some() && i.owner == page.member)
        .map(|i| (i.id.clone(), mirror.input(&i.id).eq))
        .collect();
    Captured {
        sends,
        groups,
        input_eq,
    }
}

/// Steps of the preset ramp (X15: 50 ms as five 10 ms engine ramps).
pub const RAMP_STEPS: usize = 5;
pub const RAMP_STEP_MS: u64 = 10;

fn lin_db(g: f64) -> f64 {
    if g > 0.0 { 20.0 * g.log10() } else { DB_OFF }
}

fn same(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9
}

/// The batches that move the page mix from the mirror to `target` over
/// 50 ms: amplitudes interpolate linearly; a level being muted fades to
/// silence and is muted at the last step, one being unmuted is unmuted at
/// the first step and fades in; unchanged levels are not sent. A last
/// batch sets the stored gain of levels that ended muted. Sources and groups
/// the page mix does not have are skipped and counted.
pub fn ramp(
    view: &SiteView,
    mirror: &Mirror,
    page: &Page,
    sends: &[MixSend],
    groups: &BTreeMap<GroupId, f64>,
) -> (Vec<Vec<Cmd>>, usize) {
    let mix = &page.mix;
    let mut steps: Vec<Vec<Cmd>> = vec![Vec::new(); RAMP_STEPS];
    let mut settle = Vec::new();
    let mut skipped = 0;
    let n = RAMP_STEPS as f64;
    for s in sends {
        let known = match &s.src {
            Source::Input(i) => view.input(&i.0).is_some(),
            Source::Mix(m) => view.mix(mix).is_some_and(|v| v.hears.contains(m)),
        };
        if !known {
            skipped += 1;
            continue;
        }
        let cur = mirror.level(mix, &s.src);
        if same(cur.gain_db, s.gain_db) && same(cur.pan, s.pan) && cur.muted == s.muted {
            continue;
        }
        let a = if cur.muted {
            0.0
        } else {
            db_to_lin(cur.gain_db)
        };
        let b = if s.muted { 0.0 } else { db_to_lin(s.gain_db) };
        for (k, step) in steps.iter_mut().enumerate() {
            let k = k + 1;
            let last = k == RAMP_STEPS;
            let f = k as f64 / n;
            let gain_db = if last && !s.muted {
                s.gain_db
            } else {
                lin_db(a + (b - a) * f)
            };
            let pan = if last {
                s.pan
            } else {
                cur.pan + (s.pan - cur.pan) * f
            };
            let muted = match (cur.muted, s.muted) {
                (false, true) if last => Some(true),
                (true, false) if k == 1 => Some(false),
                _ => None,
            };
            step.push(level_step(mix, &s.src, gain_db, pan, muted));
        }
        if s.muted {
            settle.push(Cmd::SetLevel {
                mix: mix.clone(),
                source: s.src.clone(),
                gain_db: Some(s.gain_db),
                pan: None,
                muted: None,
            });
        }
    }
    for (g, target) in groups {
        if view.group.as_ref() != Some(g) {
            skipped += 1;
            continue;
        }
        let cur = mirror.group(mix, g);
        if same(cur.gain_db, *target) {
            continue;
        }
        let (a, b) = (db_to_lin(cur.gain_db), db_to_lin(*target));
        for (k, step) in steps.iter_mut().enumerate() {
            let k = k + 1;
            let gain_db = if k == RAMP_STEPS {
                *target
            } else {
                lin_db(a + (b - a) * k as f64 / n)
            };
            step.push(Cmd::SetGroup {
                mix: mix.clone(),
                group: g.clone(),
                gain_db: Some(gain_db),
                muted: None,
            });
        }
    }
    if !settle.is_empty() {
        steps.push(settle);
    }
    steps.retain(|s| !s.is_empty());
    (steps, skipped)
}

fn level_step(mix: &MixId, src: &Source, gain_db: f64, pan: f64, muted: Option<bool>) -> Cmd {
    Cmd::SetLevel {
        mix: mix.clone(),
        source: src.clone(),
        gain_db: Some(gain_db),
        pan: Some(pan),
        muted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site_view::tests::test_view;
    use iem_engine_proto::{EngineMsg, Level, MixGroup, MixState, Transient};

    fn store() -> (tempfile::TempDir, BandStore) {
        let dir = tempfile::tempdir().unwrap();
        let s = BandStore::new(dir.path());
        (dir, s)
    }

    fn content(n: usize) -> Captured {
        Captured {
            sends: (0..n)
                .map(|i| MixSend {
                    src: Source::Input(InputId::new(format!("mic{}", i + 1))),
                    gain_db: -1.0,
                    pan: 0.0,
                    muted: false,
                })
                .collect(),
            ..Captured::default()
        }
    }

    #[test]
    fn presets_are_schema_3_files_with_a_limit_and_overwrite() {
        let (dir, s) = store();
        assert!(s.presets("member1").unwrap().is_empty());
        let p = s
            .save_preset("member1", "rehearsal", content(2), 100)
            .unwrap();
        assert_eq!((p.created_at, p.updated_at, p.sends.len()), (100, 100, 2));
        let text = std::fs::read_to_string(dir.path().join("presets/member1.json")).unwrap();
        let file: PresetFile = serde_json::from_str(&text).unwrap();
        assert_eq!(
            (file.format.as_str(), file.schema, file.member.as_str()),
            (PRESETS_FORMAT, 3, "member1")
        );
        let again = s
            .save_preset("member1", "rehearsal", content(3), 200)
            .unwrap();
        assert_eq!(
            (again.created_at, again.updated_at, again.sends.len()),
            (100, 200, 3)
        );
        for i in 1..MAX_PRESETS {
            s.save_preset("member1", &format!("p{i}"), content(1), 300 + i as i64)
                .unwrap();
        }
        assert_eq!(s.presets("member1").unwrap().len(), MAX_PRESETS);
        assert_eq!(
            s.save_preset("member1", "one too many", content(1), 999),
            Err(StoreError::Full)
        );
        // Overwriting at the limit is fine; newest update first.
        s.save_preset("member1", "rehearsal", content(1), 1000)
            .unwrap();
        assert_eq!(s.presets("member1").unwrap()[0].name, "rehearsal");
        assert_eq!(s.preset("member1", "p3").unwrap().unwrap().updated_at, 303);
        assert_eq!(s.preset("member1", "nope").unwrap(), None);
        assert!(s.delete_preset("member1", "p3").unwrap());
        assert!(!s.delete_preset("member1", "p3").unwrap());
        assert!(
            s.presets("member2").unwrap().is_empty(),
            "members are separate"
        );
    }

    #[test]
    fn archived_entries_are_read_only_and_foreign_files_are_refused() {
        let (dir, s) = store();
        std::fs::create_dir_all(dir.path().join("presets")).unwrap();
        let archived = Preset {
            name: "old".into(),
            archived: true,
            ..Preset::default()
        };
        std::fs::write(
            dir.path().join("presets/member1.json"),
            serde_json::to_string(&PresetFile::new("member1", vec![archived])).unwrap(),
        )
        .unwrap();
        assert_eq!(
            s.save_preset("member1", "old", content(1), 5),
            Err(StoreError::Archived)
        );
        assert_eq!(s.delete_preset("member1", "old"), Err(StoreError::Archived));
        assert!(s.preset("member1", "old").unwrap().unwrap().archived);
        // A predecessor (legacy) file is refused, not overwritten.
        let legacy =
            r#"{"rehearsal":{"name":"rehearsal","channels":{},"created_at":1,"updated_at":2}}"#;
        std::fs::write(dir.path().join("presets/member2.json"), legacy).unwrap();
        assert!(matches!(s.presets("member2"), Err(StoreError::Corrupt(..))));
        assert!(matches!(
            s.save_preset("member2", "x", content(1), 1),
            Err(StoreError::Corrupt(..))
        ));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("presets/member2.json")).unwrap(),
            legacy
        );
        let wrong_schema = r#"{"format":"iemmixer-presets","schema":2,"member":"m","presets":[]}"#;
        std::fs::write(dir.path().join("presets/member3.json"), wrong_schema).unwrap();
        assert!(matches!(s.presets("member3"), Err(StoreError::Corrupt(..))));
        assert!(matches!(s.presets("../x"), Err(StoreError::BadMember(_))));
    }

    #[test]
    fn history_prunes_the_oldest_unpinned_and_pins_with_a_label() {
        let (_dir, s) = store();
        let snap = |ts: i64, pinned: bool| Snapshot {
            timestamp: ts,
            label: "manual".into(),
            pinned,
            ..Snapshot::default()
        };
        s.add_snapshot("member1", snap(1, true)).unwrap();
        for ts in 2..=MAX_SNAPSHOTS as i64 + 5 {
            s.add_snapshot("member1", snap(ts, false)).unwrap();
        }
        let list = s.snapshots("member1").unwrap();
        assert_eq!(list.len(), MAX_SNAPSHOTS);
        assert_eq!(list[0].timestamp, MAX_SNAPSHOTS as i64 + 5, "newest first");
        assert!(list.iter().any(|x| x.timestamp == 1), "pinned kept");
        assert!(
            !list.iter().any(|x| x.timestamp == 2),
            "oldest unpinned gone"
        );
        assert!(
            s.pin_snapshot("member1", 10, true, Some("gig".into()))
                .unwrap()
        );
        let ten = s.snapshot("member1", 10).unwrap().unwrap();
        assert!(ten.pinned && ten.label == "gig");
        assert!(s.pin_snapshot("member1", 10, false, None).unwrap());
        assert!(!s.snapshot("member1", 10).unwrap().unwrap().pinned);
        assert!(!s.pin_snapshot("member1", 12345, true, None).unwrap());
        assert!(s.delete_snapshot("member1", 10).unwrap());
        assert_eq!(s.snapshot("member1", 10).unwrap(), None);
        // All pinned: nothing can be pruned.
        let mut all: Vec<Snapshot> = (0..MAX_SNAPSHOTS as i64 + 2)
            .map(|t| snap(t, true))
            .collect();
        prune(&mut all);
        assert_eq!(all.len(), MAX_SNAPSHOTS + 2);
    }

    /// A history entry's timestamp is its id (restore, pin, delete), so two
    /// entries of one second — the day's auto-snapshot and a manual save, a
    /// double tap on "Uložiť teraz" — must not share it: E2E run 36371298924
    /// saved two entries of member6 as 1790564273, and pinning, restoring or
    /// deleting the newer one reached the older.
    #[test]
    fn entries_of_one_second_keep_their_own_ids() {
        let (_dir, s) = store();
        let at = |label: &str| Snapshot {
            timestamp: 1_700_000_000,
            label: label.into(),
            ..Snapshot::default()
        };
        s.add_snapshot("member1", at(AUTO_LABEL)).unwrap();
        s.add_snapshot("member1", at("manual")).unwrap();
        s.add_snapshot("member1", at("tap")).unwrap();
        let ids = |s: &BandStore| {
            s.snapshots("member1")
                .unwrap()
                .into_iter()
                .map(|x| (x.timestamp, x.label, x.pinned))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(&s),
            vec![
                (1_700_000_002, "tap".to_string(), false),
                (1_700_000_001, "manual".to_string(), false),
                (1_700_000_000, AUTO_LABEL.to_string(), false),
            ],
            "each entry gets the next free second, newest first"
        );
        // Each id reaches its own entry.
        assert!(
            s.pin_snapshot("member1", 1_700_000_001, true, None)
                .unwrap()
        );
        assert!(s.delete_snapshot("member1", 1_700_000_000).unwrap());
        assert_eq!(
            ids(&s),
            vec![
                (1_700_000_002, "tap".to_string(), false),
                (1_700_000_001, "manual".to_string(), true),
            ]
        );
    }

    #[test]
    fn the_daily_auto_snapshot_is_found_by_its_utc_day() {
        let (_dir, s) = store();
        let day = utc_day(1_700_000_000);
        assert_eq!(day, "2023-11-14");
        assert!(!s.has_auto_snapshot_on("member1", &day).unwrap());
        s.add_snapshot(
            "member1",
            Snapshot {
                timestamp: 1_700_000_000,
                label: "manual".into(),
                ..Snapshot::default()
            },
        )
        .unwrap();
        assert!(!s.has_auto_snapshot_on("member1", &day).unwrap());
        s.add_snapshot(
            "member1",
            Snapshot {
                timestamp: 1_700_000_100,
                label: AUTO_LABEL.into(),
                ..Snapshot::default()
            },
        )
        .unwrap();
        assert!(s.has_auto_snapshot_on("member1", &day).unwrap());
        assert!(!s.has_auto_snapshot_on("member1", "2023-11-15").unwrap());
    }

    #[test]
    fn customizations_round_trip() {
        let (_dir, s) = store();
        assert!(s.customization("member1").unwrap().pinned.is_empty());
        s.save_customization(
            "member1",
            vec![Source::Input(InputId::new("mic1"))],
            vec![Source::Mix(MixId::new("member2"))],
        )
        .unwrap();
        let c = s.customization("member1").unwrap();
        assert_eq!(c.format, CUSTOMIZATION_FORMAT);
        assert_eq!(c.pinned, vec![Source::Input(InputId::new("mic1"))]);
        assert_eq!(c.hidden, vec![Source::Mix(MixId::new("member2"))]);
    }

    #[test]
    fn customizations_are_per_member_and_a_save_replaces_them() {
        let (_dir, s) = store();
        let mic = |n: u32| Source::Input(InputId::new(format!("mic{n}")));
        s.save_customization("member1", vec![mic(1)], vec![])
            .unwrap();
        s.save_customization("member2", vec![], vec![mic(5)])
            .unwrap();
        let one = s.customization("member1").unwrap();
        let two = s.customization("member2").unwrap();
        assert_eq!((one.pinned, one.hidden), (vec![mic(1)], vec![]));
        assert_eq!((two.pinned, two.hidden), (vec![], vec![mic(5)]));
        // A save is the member's whole list, not an addition to it.
        s.save_customization("member1", vec![mic(5)], vec![])
            .unwrap();
        let one = s.customization("member1").unwrap();
        assert_eq!((one.pinned, one.hidden), (vec![mic(5)], vec![]));
        s.save_customization("member1", vec![], vec![]).unwrap();
        let one = s.customization("member1").unwrap();
        assert!(one.pinned.is_empty() && one.hidden.is_empty());
        assert_eq!(one.member, "member1");
        assert_eq!(
            s.customization("member2").unwrap().hidden,
            vec![mic(5)],
            "the other member's file is untouched"
        );
    }

    #[test]
    fn a_missing_file_reads_as_its_members_empty_file() {
        let (_dir, s) = store();
        assert_eq!(
            s.customization("member1").unwrap(),
            CustomizationFile::new("member1", Vec::new(), Vec::new())
        );
    }

    #[test]
    fn an_unreadable_file_is_an_error_not_an_empty_one() {
        let (dir, s) = store();
        // A folder where the file should be: reading it fails, not "not found".
        std::fs::create_dir_all(dir.path().join("customizations/member1.json")).unwrap();
        assert!(matches!(s.customization("member1"), Err(StoreError::Io(_))));
    }

    fn mirror(f: impl FnOnce(&mut MixState)) -> Mirror {
        let mut state = MixState::default();
        f(&mut state);
        let mut m = Mirror::default();
        m.apply(&EngineMsg::State {
            rev: 1,
            state,
            transient: Transient::default(),
        });
        m
    }

    #[test]
    fn capture_takes_the_page_mix_and_the_members_input_eq() {
        let v = test_view();
        let m = mirror(|s| {
            let mx = s.mixes.entry(MixId::new("member1")).or_default();
            mx.inputs.insert(
                InputId::new("keys"),
                Level {
                    gain_db: -7.0,
                    pan: 0.25,
                    muted: true,
                },
            );
            mx.groups.insert(
                GroupId::new("stems"),
                MixGroup {
                    gain_db: -2.0,
                    ..MixGroup::default()
                },
            );
            s.inputs.entry(InputId::new("mic1")).or_default().eq.gain_db = 1.5;
        });
        let c = capture(&v, &m, &v.page("member1").unwrap());
        assert_eq!(c.sends.len(), 24 + 8);
        let keys = c
            .sends
            .iter()
            .find(|s| s.src == Source::Input(InputId::new("keys")))
            .unwrap();
        assert_eq!((keys.gain_db, keys.pan, keys.muted), (-7.0, 0.25, true));
        assert_eq!(c.sends[24].src, Source::Mix(MixId::new("member2")));
        assert_eq!(c.groups[&GroupId::new("stems")], -2.0);
        assert_eq!(c.input_eq.len(), 1);
        assert_eq!(c.input_eq[&InputId::new("mic1")].gain_db, 1.5);
        // The translator page has no member: no input EQ.
        assert!(
            capture(&v, &m, &v.page("translator").unwrap())
                .input_eq
                .is_empty()
        );
    }

    fn level_of(c: &Cmd) -> (f64, f64, Option<bool>) {
        match c {
            Cmd::SetLevel {
                gain_db: Some(g),
                pan: Some(p),
                muted,
                ..
            } => (*g, *p, *muted),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn ramp_interpolates_amplitudes_and_lands_on_the_target() {
        let v = test_view();
        let p = v.page("member1").unwrap();
        let m = mirror(|s| {
            let mx = s.mixes.entry(MixId::new("member1")).or_default();
            mx.inputs.insert(
                InputId::new("mic1"),
                Level {
                    gain_db: 0.0,
                    pan: -1.0,
                    muted: false,
                },
            );
            mx.inputs.insert(
                InputId::new("mic2"),
                Level {
                    gain_db: -6.0,
                    pan: 0.0,
                    muted: false,
                },
            );
        });
        let sends = vec![
            MixSend {
                src: Source::Input(InputId::new("mic1")),
                gain_db: -6.020599913279624,
                pan: 1.0,
                muted: false,
            },
            // Unchanged: not sent.
            MixSend {
                src: Source::Input(InputId::new("mic2")),
                gain_db: -6.0,
                pan: 0.0,
                muted: false,
            },
            // Unknown here: skipped.
            MixSend {
                src: Source::Input(InputId::new("gone")),
                gain_db: 0.0,
                pan: 0.0,
                muted: false,
            },
        ];
        let groups = BTreeMap::from([(GroupId::new("stems"), 6.0), (GroupId::new("other"), 0.0)]);
        let (steps, skipped) = ramp(&v, &m, &p, &sends, &groups);
        assert_eq!(skipped, 2);
        assert_eq!(steps.len(), RAMP_STEPS);
        for (k, step) in steps.iter().enumerate() {
            assert_eq!(step.len(), 2, "mic1 and the stems strip");
            let (g, pan, muted) = level_of(&step[0]);
            let f = (k + 1) as f64 / 5.0;
            let lin = 1.0 + (0.5 - 1.0) * f;
            assert!((g - 20.0 * lin.log10()).abs() < 1e-9, "step {k}: {g}");
            assert!((pan - (-1.0 + 2.0 * f)).abs() < 1e-12);
            assert_eq!(muted, None);
        }
        let (g, pan, _) = level_of(&steps[4][0]);
        assert_eq!((g, pan), (-6.020599913279624, 1.0), "exactly the target");
        assert_eq!(
            steps[4][1],
            Cmd::SetGroup {
                mix: MixId::new("member1"),
                group: GroupId::new("stems"),
                gain_db: Some(6.0),
                muted: None
            }
        );
    }

    #[test]
    fn ramp_orders_mutes_so_nothing_jumps() {
        let v = test_view();
        let p = v.page("member2").unwrap();
        let m = mirror(|s| {
            let mx = s.mixes.entry(MixId::new("member2")).or_default();
            mx.inputs.insert(
                InputId::new("mic1"),
                Level {
                    gain_db: 0.0,
                    pan: 0.0,
                    muted: false,
                },
            );
            mx.inputs.insert(
                InputId::new("mic2"),
                Level {
                    gain_db: 0.0,
                    pan: 0.0,
                    muted: true,
                },
            );
        });
        let sends = vec![
            // Being muted, stored louder: fades out, muted last, gain settled after.
            MixSend {
                src: Source::Input(InputId::new("mic1")),
                gain_db: 6.0,
                pan: 0.0,
                muted: true,
            },
            // Being unmuted: unmuted first, fades in from silence.
            MixSend {
                src: Source::Input(InputId::new("mic2")),
                gain_db: 0.0,
                pan: 0.0,
                muted: false,
            },
        ];
        let (steps, skipped) = ramp(&v, &m, &p, &sends, &BTreeMap::new());
        assert_eq!(skipped, 0);
        assert_eq!(steps.len(), RAMP_STEPS + 1);
        let mic1: Vec<(f64, f64, Option<bool>)> =
            steps[..5].iter().map(|s| level_of(&s[0])).collect();
        assert_eq!(mic1[0].2, None);
        assert_eq!(mic1[4], (DB_OFF, 0.0, Some(true)), "silent when muted");
        assert!(mic1[3].0 < mic1[0].0, "fading out");
        let mic2: Vec<(f64, f64, Option<bool>)> =
            steps[..5].iter().map(|s| level_of(&s[1])).collect();
        assert_eq!(mic2[0].2, Some(false));
        assert!(
            (mic2[0].0 - 20.0 * 0.2f64.log10()).abs() < 1e-9,
            "starts quiet"
        );
        assert_eq!(mic2[4], (0.0, 0.0, None));
        assert_eq!(
            steps[5],
            vec![Cmd::SetLevel {
                mix: MixId::new("member2"),
                source: Source::Input(InputId::new("mic1")),
                gain_db: Some(6.0),
                pan: None,
                muted: None
            }]
        );
        // Nothing to change: no batch at all.
        let (none, _) = ramp(&v, &m, &p, &[], &BTreeMap::new());
        assert!(none.is_empty());
        assert_eq!(lin_db(0.0), DB_OFF);
        assert_eq!(lin_db(1.0), 0.0);
    }

    #[test]
    fn the_stems_fader_ramps_through_linear_amplitudes() {
        let v = test_view();
        let p = v.page("member1").unwrap();
        let m = mirror(|s| {
            s.mixes
                .entry(MixId::new("member1"))
                .or_default()
                .groups
                .insert(
                    GroupId::new("stems"),
                    MixGroup {
                        gain_db: -6.0,
                        ..MixGroup::default()
                    },
                );
        });
        let groups = BTreeMap::from([(GroupId::new("stems"), 6.0)]);
        let (steps, skipped) = ramp(&v, &m, &p, &[], &groups);
        assert_eq!((steps.len(), skipped), (RAMP_STEPS, 0));
        let (a, b) = (db_to_lin(-6.0), db_to_lin(6.0));
        for (k, step) in steps.iter().enumerate() {
            let f = (k + 1) as f64 / RAMP_STEPS as f64;
            let want = 20.0 * (a + (b - a) * f).log10();
            match step.as_slice() {
                [
                    Cmd::SetGroup {
                        gain_db: Some(g),
                        muted: None,
                        ..
                    },
                ] => assert!((g - want).abs() < 1e-9, "step {k}: {g} dB, not {want}"),
                other => panic!("{other:?}"),
            }
        }
    }
}
