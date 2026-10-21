//! The lineups' files (#58 design §6, §7, §11). `<config>/lineups/<id>/`
//! holds a lineup's stored mixer state (`state.json`, a backup's shape: the
//! state and its people's pins and hides) and its people's presets, history
//! and pins/hides (the band store's `presets/`, `snapshots/` and
//! `customizations/`, keyed by person). `lineups/active.json` names the
//! active lineup; `lineups/switch.json` exists only while a switch runs (its
//! intent, replayed after a crash). Every write is a synced temporary file
//! renamed over the old one.
//!
//! Also the one-time migration of the legacy layout (no `lineups/`): today's
//! per-member files become the main lineup's, and the members who are no
//! person go to the people archive.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use iem_core::backup::MixerBackup;
use iem_core::config::Config;
use iem_core::roster::{Roster, valid_lineup_id};
use serde::{Deserialize, Serialize};

use crate::people_archive::archive_person;
use crate::pin_store::{ENGINEER_ID, PinStore};

/// `<config>/lineups/`: one directory per lineup.
pub const LINEUPS_DIR: &str = "lineups";
/// `{"format":"iemmixer-active-lineup","lineup":id,"since":ts}`.
pub const ACTIVE_FILE: &str = "lineups/active.json";
/// `{"from":id,"to":id}`: present only while a switch runs.
pub const SWITCH_FILE: &str = "lineups/switch.json";
/// The active file's format.
pub const ACTIVE_FORMAT: &str = "iemmixer-active-lineup";
/// The migration builds `lineups/` here and renames it as its last step.
pub const STAGING_DIR: &str = "lineups.migrating";
/// The band store's per-person files, one directory each.
pub const KINDS: [&str; 3] = ["presets", "snapshots", "customizations"];

const ACTIVE_NAME: &str = "active.json";
const SWITCH_NAME: &str = "switch.json";
const STATE_NAME: &str = "state.json";

/// A running switch: from the active lineup to another (#58 design §7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchIntent {
    pub from: String,
    pub to: String,
}

#[derive(Serialize, Deserialize)]
struct ActiveFile {
    format: String,
    lineup: String,
    since: i64,
}

/// The `lineups/` directory of a config directory.
#[derive(Debug, Clone)]
pub struct LineupStore {
    dir: PathBuf,
}

fn invalid(what: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}

fn check_id(id: &str) -> io::Result<()> {
    if valid_lineup_id(id) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("'{id}' is not a lineup id"),
        ))
    }
}

/// The file's text; `None` when it does not exist.
fn read_optional(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io::Error::new(e.kind(), format!("{}: {e}", path.display()))),
    }
}

/// Writes `data` to `path` through a synced temporary file renamed over it:
/// a crash leaves the old file or the new one, never a torn one.
fn write_durable(path: &Path, data: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    let mut file = fs::File::create(&tmp)?;
    file.write_all(data.as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::rename(tmp, path)
}

impl LineupStore {
    pub fn new(config_dir: &Path) -> Self {
        Self::at(config_dir.join(LINEUPS_DIR))
    }

    /// A store over another directory (the migration's staging one).
    fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The active lineup; `None` before any was recorded.
    pub fn active(&self) -> io::Result<Option<String>> {
        let path = self.dir.join(ACTIVE_NAME);
        let Some(text) = read_optional(&path)? else {
            return Ok(None);
        };
        let file: ActiveFile =
            serde_json::from_str(&text).map_err(|e| invalid(format!("{}: {e}", path.display())))?;
        if file.format != ACTIVE_FORMAT {
            return Err(invalid(format!(
                "{}: format {:?} is not {ACTIVE_FORMAT:?}",
                path.display(),
                file.format
            )));
        }
        check_id(&file.lineup).map_err(|e| invalid(format!("{}: {e}", path.display())))?;
        Ok(Some(file.lineup))
    }

    /// Records `id` as the active lineup since `since` (Unix seconds).
    pub fn set_active(&self, id: &str, since: i64) -> io::Result<()> {
        check_id(id)?;
        let file = ActiveFile {
            format: ACTIVE_FORMAT.into(),
            lineup: id.into(),
            since,
        };
        let json = serde_json::to_string(&file).map_err(io::Error::other)?;
        write_durable(&self.dir.join(ACTIVE_NAME), &json)
    }

    /// The lineup's stored state; `None` when it has none yet.
    pub fn state(&self, id: &str) -> io::Result<Option<MixerBackup>> {
        check_id(id)?;
        let path = self.data_dir(id).join(STATE_NAME);
        let Some(text) = read_optional(&path)? else {
            return Ok(None);
        };
        MixerBackup::parse(&text)
            .map(Some)
            .map_err(|e| invalid(format!("{}: {e}", path.display())))
    }

    pub fn save_state(&self, id: &str, backup: &MixerBackup) -> io::Result<()> {
        check_id(id)?;
        let json = serde_json::to_string_pretty(backup).map_err(io::Error::other)?;
        write_durable(&self.data_dir(id).join(STATE_NAME), &json)
    }

    /// The running switch's intent; `None` when no switch runs.
    pub fn intent(&self) -> io::Result<Option<SwitchIntent>> {
        let path = self.dir.join(SWITCH_NAME);
        let Some(text) = read_optional(&path)? else {
            return Ok(None);
        };
        let intent: SwitchIntent =
            serde_json::from_str(&text).map_err(|e| invalid(format!("{}: {e}", path.display())))?;
        check_id(&intent.from)
            .and_then(|()| check_id(&intent.to))
            .map_err(|e| invalid(format!("{}: {e}", path.display())))?;
        Ok(Some(intent))
    }

    pub fn set_intent(&self, i: &SwitchIntent) -> io::Result<()> {
        check_id(&i.from)?;
        check_id(&i.to)?;
        let json = serde_json::to_string(i).map_err(io::Error::other)?;
        write_durable(&self.dir.join(SWITCH_NAME), &json)
    }

    /// Ends a switch: its intent is gone (none is fine).
    pub fn clear_intent(&self) -> io::Result<()> {
        match fs::remove_file(self.dir.join(SWITCH_NAME)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// The lineup's directory: its stored state and its people's files (the
    /// band store's data directory). `id` is a roster's lineup id.
    pub fn data_dir(&self, id: &str) -> PathBuf {
        self.dir.join(id)
    }
}

/// What the migration did, in ids only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationReport {
    /// People whose own files moved into the main lineup.
    pub moved: Vec<String>,
    /// `(person, former member)`: whose files the person took over.
    pub taken_over: Vec<(String, String)>,
    /// Former members who are no person, gone to the archive.
    pub archived: Vec<String>,
}

/// A step of the migration a test can fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// The rename of the staged `lineups/` into place.
    Commit,
}

fn file_of(kind: &str, id: &str) -> PathBuf {
    Path::new(kind).join(format!("{id}.json"))
}

/// Copies `from` to `to` (created, synced) unless `to` exists or `from` does
/// not; `true` when it copied.
fn copy_durable(from: &Path, to: &Path) -> io::Result<bool> {
    if !from.try_exists()? || to.try_exists()? {
        return Ok(false);
    }
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(from, to)?;
    // Synced through a handle with write access (Windows flushes only so).
    fs::OpenOptions::new().write(true).open(to)?.sync_all()?;
    Ok(true)
}

/// Moves the legacy layout (`<config>/{presets,snapshots,customizations}/
/// <member>.json`) into the main lineup, once, when `lineups/` is absent
/// (#58 design §11): each legacy member's files become the main lineup's
/// under their id; a person with `takes_over = X` gets X's files (never X's
/// PIN or photo; their own files, if any, stay theirs); every former member
/// who is no person (and every `takes_over` one) then goes to the archive;
/// `active.json` names the main lineup.
///
/// The new layout is staged in `lineups.migrating/` (copies only, the legacy
/// files untouched) and renamed to `lineups/` as the commit, so a crash
/// leaves the legacy layout or the migrated one; a leftover staging
/// directory is removed and the migration runs again. Only after the commit
/// are the legacy copies removed and the former members archived. With
/// `lineups/` present it does nothing: a crash after the commit leaves only
/// stale legacy copies, and former members who are no person (no login, no
/// tile) still unarchived.
pub fn migrate_legacy(
    config_dir: &Path,
    roster: &Roster,
    cfg: &Config,
    pins: &mut PinStore,
) -> io::Result<MigrationReport> {
    migrate_with(config_dir, roster, cfg, pins, &|_| Ok(()))
}

pub(crate) fn migrate_with(
    config_dir: &Path,
    roster: &Roster,
    cfg: &Config,
    pins: &mut PinStore,
    fault: &dyn Fn(Step) -> io::Result<()>,
) -> io::Result<MigrationReport> {
    let mut report = MigrationReport::default();
    let live = config_dir.join(LINEUPS_DIR);
    if live.try_exists()? {
        return Ok(report);
    }
    let legacy: Vec<&str> = cfg
        .members
        .iter()
        .map(|m| m.id.as_str())
        .filter(|id| *id != ENGINEER_ID)
        .collect();
    let takers: Vec<(&str, &str)> = roster
        .people
        .iter()
        .filter_map(|p| p.takes_over.as_deref().map(|x| (p.id.as_str(), x)))
        .collect();
    let staging = config_dir.join(STAGING_DIR);
    if staging.try_exists()? {
        tracing::warn!("an unfinished migration's staging directory: removed, starting again");
        fs::remove_dir_all(&staging)?;
    }
    let store = LineupStore::at(staging.clone());
    let main = store.data_dir(&roster.main().id);
    // Each member's own files under their own id: a person's, and a former
    // member's whom nobody takes over (for the archive).
    for &id in &legacy {
        let person = roster.person(id).is_some();
        if !person && takers.iter().any(|&(_, x)| x == id) {
            continue;
        }
        let mut copied = false;
        for kind in KINDS {
            let file = file_of(kind, id);
            copied |= copy_durable(&config_dir.join(&file), &main.join(file))?;
        }
        if copied && person {
            report.moved.push(id.to_string());
        }
    }
    // Taken-over files under the taker's id; one the taker already has stays
    // the former member's, for the archive.
    for &(person, from) in &takers {
        let mut copied = false;
        for kind in KINDS {
            let src = config_dir.join(file_of(kind, from));
            if copy_durable(&src, &main.join(file_of(kind, person)))? {
                copied = true;
            } else {
                copy_durable(&src, &main.join(file_of(kind, from)))?;
            }
        }
        if copied {
            report
                .taken_over
                .push((person.to_string(), from.to_string()));
        }
    }
    store.set_active(&roster.main().id, chrono::Utc::now().timestamp())?;
    fault(Step::Commit)?;
    fs::rename(staging, live)?;
    tracing::info!(
        moved = report.moved.len(),
        taken_over = report.taken_over.len(),
        main = %roster.main().id,
        "legacy band files migrated into the main lineup"
    );
    let former = legacy.iter().copied().chain(takers.iter().map(|&(_, x)| x));
    for id in former {
        for kind in KINDS {
            if let Err(e) = fs::remove_file(config_dir.join(file_of(kind, id)))
                && e.kind() != io::ErrorKind::NotFound
            {
                return Err(e);
            }
        }
        if roster.person(id).is_none() && archive_person(config_dir, pins, id)? {
            tracing::info!(member = %id, "former member archived");
            report.archived.push(id.to_string());
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests;
