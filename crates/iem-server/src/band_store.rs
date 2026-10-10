//! Presets, history and pins/hides in the band schema-3 files (F8, F13, F14;
//! S4 design note §3.4, S5 design note §6): `presets/<member>.json`,
//! `snapshots/<member>.json`, `customizations/<member>.json` in the config
//! directory. A file of another format or schema is an error and is never
//! overwritten. Archived entries (a renamed member's history, D8) are
//! loadable but never changed.
//!
//! Also the two pure halves of presets and snapshots: `capture` (the page
//! mix from the mirror) and `ramp` (applying one as a 50 ms ramp).

use std::collections::{BTreeMap, BTreeSet};
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

    /// Adds a snapshot and prunes the oldest unpinned beyond 50; returns the
    /// timestamp it is stored under. The timestamp is the entry's id
    /// (restore, pin, delete), so an entry whose second is taken moves to the
    /// next free second.
    pub fn add_snapshot(&self, member: &str, mut snapshot: Snapshot) -> Result<i64, StoreError> {
        let _g = self.guard();
        let mut file = self.read::<SnapshotFile>("snapshots", member)?;
        // Steps over the taken seconds from the entry's own on: the set is
        // sorted and holds each second once (an older file may hold one
        // twice), so the walk ends at the first gap.
        let taken: BTreeSet<i64> = file.snapshots.iter().map(|s| s.timestamp).collect();
        for &t in taken.range(snapshot.timestamp..) {
            if t != snapshot.timestamp {
                break;
            }
            snapshot.timestamp += 1;
        }
        let id = snapshot.timestamp;
        file.snapshots.push(snapshot);
        prune(&mut file.snapshots);
        self.write("snapshots", member, &file)?;
        Ok(id)
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
mod tests;
