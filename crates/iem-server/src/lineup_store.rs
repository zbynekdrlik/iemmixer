//! The lineups' files (#58 design §6, §7, §11). `<config>/lineups/<id>/`
//! holds a lineup's stored mixer state (`state.json`, a backup's shape: the
//! state and its people's pins and hides) and its people's presets, history
//! and pins/hides (the band store's `presets/`, `snapshots/` and
//! `customizations/`, keyed by person). `lineups/active.json` names the
//! active lineup; `lineups/switch.json` exists only while a switch runs (its
//! intent, replayed after a crash); `lineups/applied-roster.toml` is a copy
//! of the roster the states follow, and `lineups/running.json` says what the
//! engine's running state follows ([`Running`]). A stored state carries what
//! it follows itself (`MixerBackup::follows`, `silenced`). Every write is a
//! synced temporary file renamed over the old one.
//!
//! Also the one-time migration of the legacy layout (no
//! `lineups/active.json`): today's per-member files and the engineer's
//! become the main lineup's, and the members who are no person are removed
//! (`people_removal`: their files, photo and PIN hash deleted); the work
//! left after its commit is kept in `lineups/migration.json` until done, so
//! a crash anywhere is finished by the next start.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use iem_core::backup::MixerBackup;
use iem_core::config::Config;
use iem_core::roster::{Roster, valid_lineup_id};
use serde::{Deserialize, Serialize};

use crate::people_removal::remove_person;
use crate::pin_store::{ENGINEER_ID, PinStore};

/// `<config>/lineups/`: one directory per lineup.
pub const LINEUPS_DIR: &str = "lineups";
/// `{"format":"iemmixer-active-lineup","lineup":id,"since":ts}`.
pub const ACTIVE_FILE: &str = "lineups/active.json";
/// `{"from":id,"to":id}`: present only while a switch runs.
pub const SWITCH_FILE: &str = "lineups/switch.json";
/// A copy of the roster the states follow (#58 review): what a reload or the
/// start moves people's mixes from, and what the start falls back to when
/// `roster.toml` cannot be used.
pub const APPLIED_FILE: &str = "lineups/applied-roster.toml";
/// What the engine's running state follows ([`Running`]).
pub const RUNNING_FILE: &str = "lineups/running.json";
/// Every roster a state may follow, `<sha256>.toml` (#58 third review):
/// written when a roster comes into use, never deleted here. Its name is no
/// lineup id (the dot), so it is no lineup's folder (#58 fourth review).
pub const ROSTERS_DIR: &str = "lineups/rosters.kept";
/// The active file's format.
pub const ACTIVE_FORMAT: &str = "iemmixer-active-lineup";
/// The migration builds `lineups/` here and renames it as its commit.
pub const STAGING_DIR: &str = "lineups.migrating";
/// The work a committed migration still has to do (deletes, removals);
/// gone once done.
pub const PENDING_FILE: &str = "lineups/migration.json";
/// The pending file's format.
pub const PENDING_FORMAT: &str = "iemmixer-migration";
/// The band store's per-person files, one directory each.
pub const KINDS: [&str; 3] = ["presets", "snapshots", "customizations"];

const ACTIVE_NAME: &str = "active.json";
const SWITCH_NAME: &str = "switch.json";
const STATE_NAME: &str = "state.json";
const APPLIED_NAME: &str = "applied-roster.toml";
const RUNNING_NAME: &str = "running.json";
const ROSTERS_NAME: &str = "rosters.kept";
const PENDING_NAME: &str = "migration.json";

/// How often a rename or a delete a transient Windows error fails is tried,
/// and the pause between tries (the engine's lesson of #32: a scanner or the
/// indexer holds a file for a moment). Never a silent fallback: the last
/// error is returned.
const RETRY_TRIES: u32 = 5;
const RETRY_PAUSE: std::time::Duration = std::time::Duration::from_millis(200);

/// A running switch: from the active lineup to another (#58 design §7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchIntent {
    pub from: String,
    pub to: String,
}

/// What the engine's running state follows (#58 re-review), written right
/// after every import that changes it: whose state it is, the SHA-256 of
/// the roster file it follows (none: not known, a backup's restored state
/// or a state from before), and the sets the lineup's rules silenced in it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Running {
    pub lineup: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follows: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub silenced: BTreeSet<String>,
    /// A roster's import sent and not answered yet (#58 third review),
    /// written before it: what the running state follows once the engine
    /// holds the state sent (`roster_watch::running_of` resolves it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<Pending>,
}

/// What the running state follows once an import went in
/// ([`Running::pending`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub follows: String,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub silenced: BTreeSet<String>,
    /// The SHA-256 of the state sent (`roster_watch::state_digest`).
    pub digest: String,
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

/// Whether a failed rename or delete is tried again: on Windows only, after
/// access denied (5), a sharing violation (32) or a lock violation (33),
/// what a scanner or the indexer holding a file for a moment gives.
fn transient(e: &io::Error, windows: bool) -> bool {
    windows && matches!(e.raw_os_error(), Some(5 | 32 | 33))
}

/// Runs `op` until it succeeds, fails with an error `again` refuses, or ran
/// `tries` times; `pause` runs between two tries. The last error is
/// returned.
fn retry<T>(
    tries: u32,
    mut op: impl FnMut() -> io::Result<T>,
    again: impl Fn(&io::Error) -> bool,
    mut pause: impl FnMut(),
) -> io::Result<T> {
    let mut n = 1;
    loop {
        match op() {
            Err(e) if n < tries && again(&e) => {
                tracing::warn!(error = %e, attempt = n, "a file operation failed for a moment: trying again");
                pause();
                n += 1;
            }
            done => return done,
        }
    }
}

/// `op` (a rename or a delete), tried again while it fails with a
/// transient error ([`transient`]; `windows`: the server's OS, which tests
/// set) up to [`RETRY_TRIES`] times, [`RETRY_PAUSE`] between tries.
pub(crate) fn retrying<T>(windows: bool, op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    retry(
        RETRY_TRIES,
        op,
        |e| transient(e, windows),
        || std::thread::sleep(RETRY_PAUSE),
    )
}

/// `fs::rename`, tried again on a transient Windows error ([`transient`]).
fn rename_retrying(from: &Path, to: &Path) -> io::Result<()> {
    retrying(cfg!(windows), || fs::rename(from, to))
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
    rename_retrying(&tmp, path)
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

    /// What the running state follows; `None` before it was recorded.
    pub fn running(&self) -> io::Result<Option<Running>> {
        let path = self.dir.join(RUNNING_NAME);
        let Some(text) = read_optional(&path)? else {
            return Ok(None);
        };
        let running: Running =
            serde_json::from_str(&text).map_err(|e| invalid(format!("{}: {e}", path.display())))?;
        check_id(&running.lineup).map_err(|e| invalid(format!("{}: {e}", path.display())))?;
        Ok(Some(running))
    }

    pub fn set_running(&self, running: &Running) -> io::Result<()> {
        check_id(&running.lineup)?;
        let json = serde_json::to_string(running).map_err(io::Error::other)?;
        write_durable(&self.dir.join(RUNNING_NAME), &json)
    }

    /// Keeps `text`, whose SHA-256 is `sha256`, as a roster a state may
    /// follow ([`ROSTERS_DIR`]); never deleted here. One kept already is not
    /// written again: the file named by its SHA-256 holds it (written
    /// durably, renamed into place whole).
    pub fn keep_roster(&self, sha256: &str, text: &str) -> io::Result<()> {
        let path = self
            .kept(sha256)
            .ok_or_else(|| invalid(format!("'{sha256}' is no SHA-256")))?;
        if path.try_exists()? {
            return Ok(());
        }
        write_durable(&path, text)
    }

    /// The kept roster whose SHA-256 is `sha256`; `None` when none is kept
    /// (or `sha256` is no SHA-256, as a marker read back may be).
    pub fn kept_roster(&self, sha256: &str) -> io::Result<Option<String>> {
        match self.kept(sha256) {
            Some(path) => read_optional(&path),
            None => Ok(None),
        }
    }

    fn kept(&self, sha256: &str) -> Option<PathBuf> {
        let hex = sha256.len() == 64
            && sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        hex.then(|| self.dir.join(ROSTERS_NAME).join(format!("{sha256}.toml")))
    }

    /// The roster the states follow ([`APPLIED_FILE`]); `None` before one
    /// was recorded.
    pub fn applied_roster(&self) -> io::Result<Option<String>> {
        read_optional(&self.dir.join(APPLIED_NAME))
    }

    /// Records `text` as the roster the states follow.
    pub fn set_applied_roster(&self, text: &str) -> io::Result<()> {
        write_durable(&self.dir.join(APPLIED_NAME), text)
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
        debug_assert!(
            valid_lineup_id(id),
            "a lineup's directory needs a lineup id"
        );
        self.dir.join(id)
    }
}

/// What the migration did, in ids only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationReport {
    /// People (and the engineer) whose own files moved into the main lineup.
    pub moved: Vec<String>,
    /// `(person, former member)`: whose files the person took over.
    pub taken_over: Vec<(String, String)>,
    /// Former members who are no person, removed (their data deleted).
    pub removed: Vec<String>,
}

/// A step of the migration a test can fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// The rename of the staged `lineups/` into place.
    Commit,
    /// The pending work after the commit (deletes, removals).
    Finish,
}

/// The work a committed migration still has to do, kept in
/// [`PENDING_FILE`] until it is done.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MigrationPending {
    format: String,
    /// Ids whose legacy files (`<kind>/<id>.json`) are copies now: deleted.
    delete: Vec<String>,
    /// Former members who are no person: removed. An older server wrote
    /// them as `archive` (#58 decision 6, overruled 2026-10-24); that list
    /// is read the same.
    #[serde(alias = "archive")]
    remove: Vec<String>,
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

/// Why `roster` cannot migrate the legacy files: its problems, and a
/// `takes_over` that names a person (a person's files are their own). Read
/// only for the migration: once it is done, `takes_over` means nothing.
pub fn migration_problems(roster: &Roster) -> Vec<String> {
    let mut out = roster.problems();
    for (i, p) in roster.people.iter().enumerate() {
        if let Some(t) = &p.takes_over
            && roster.person(t).is_some()
        {
            out.push(format!(
                "person #{}: takes_over names a person, whose files are their own",
                i + 1
            ));
        }
    }
    out
}

/// Moves the legacy layout (`<config>/{presets,snapshots,customizations}/
/// <member>.json`) into the main lineup, once (#58 design §11): each legacy
/// member's files and the engineer's become the main lineup's under their
/// id; a person with `takes_over = X` gets X's files (never X's PIN or
/// photo; their own files, if any, stay theirs); every former member who is
/// no person (and every `takes_over` one) is then removed
/// (`people_removal::remove_person`: files, photo and PIN hash deleted);
/// `active.json` names the main lineup. A roster with
/// [`migration_problems`] is refused before anything is written.
///
/// The new layout is staged in `lineups.migrating/` (copies only, the legacy
/// files untouched), with the work left after the commit in
/// `migration.json`, and renamed to `lineups/` as the commit: a crash before
/// it leaves the legacy layout (the staging directory is removed and the
/// migration runs again), a crash after it is finished by the next call.
/// "Migrated" is `lineups/active.json`; a `lineups/` without it is refused
/// (nothing commits over it). Once migrated, a call only finishes pending
/// work: call it at every start.
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
    if live.join(ACTIVE_NAME).try_exists()? {
        if roster.people.iter().any(|p| p.takes_over.is_some()) {
            tracing::info!(
                "the roster's takes_over is read only by the one-time migration, done: it can go"
            );
        }
        report.removed = finish(config_dir, roster, pins)?;
        return Ok(report);
    }
    let problems = migration_problems(roster);
    if !problems.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "the roster cannot migrate the legacy band files: {}",
                problems.join("; ")
            ),
        ));
    }
    if live.try_exists()? {
        return Err(invalid(format!(
            "{} exists without {ACTIVE_NAME}: no migration commits over it (move it aside)",
            live.display()
        )));
    }
    // The legacy members, and the engineer: their files are per lineup too.
    let legacy: Vec<&str> = cfg
        .members
        .iter()
        .map(|m| m.id.as_str())
        .filter(|id| *id != ENGINEER_ID)
        .chain([ENGINEER_ID])
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
    let stays = |id: &str| id == ENGINEER_ID || roster.person(id).is_some();
    // Each member's own files under their own id: a person's, the
    // engineer's, and a former member's whom nobody takes over (deleted by
    // the pending work, unless they are a person again by then).
    for &id in &legacy {
        if !stays(id) && takers.iter().any(|&(_, x)| x == id) {
            continue;
        }
        let mut copied = false;
        for kind in KINDS {
            let file = file_of(kind, id);
            copied |= copy_durable(&config_dir.join(&file), &main.join(file))?;
        }
        if copied && stays(id) {
            report.moved.push(id.to_string());
        }
    }
    // Taken-over files under the taker's id; one the taker already has stays
    // the former member's (deleted with them by the pending work).
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
    let former: Vec<&str> = legacy
        .iter()
        .copied()
        .chain(takers.iter().map(|&(_, x)| x))
        .collect();
    let pending = MigrationPending {
        format: PENDING_FORMAT.into(),
        delete: former.iter().copied().map(str::to_string).collect(),
        remove: former
            .iter()
            .copied()
            .filter(|&id| !stays(id))
            .map(str::to_string)
            .collect(),
    };
    let json = serde_json::to_string_pretty(&pending).map_err(io::Error::other)?;
    write_durable(&staging.join(PENDING_NAME), &json)?;
    store.set_active(&roster.main().id, chrono::Utc::now().timestamp())?;
    fault(Step::Commit)?;
    rename_retrying(&staging, &live)?;
    tracing::info!(
        moved = report.moved.len(),
        taken_over = report.taken_over.len(),
        main = %roster.main().id,
        "legacy band files migrated into the main lineup"
    );
    fault(Step::Finish)?;
    report.removed = finish(config_dir, roster, pins)?;
    Ok(report)
}

/// Finishes a committed migration's pending work, if any: deletes the
/// legacy copies, removes the former members who are (still) no person
/// (their files, photo and PIN hash deleted), then removes
/// [`PENDING_FILE`]. Each step may run again, so a crash or a failed delete
/// in it is finished by the next call. The ids removed.
fn finish(config_dir: &Path, roster: &Roster, pins: &mut PinStore) -> io::Result<Vec<String>> {
    let path = config_dir.join(PENDING_FILE);
    let Some(text) = read_optional(&path)? else {
        return Ok(Vec::new());
    };
    let pending: MigrationPending =
        serde_json::from_str(&text).map_err(|e| invalid(format!("{}: {e}", path.display())))?;
    if pending.format != PENDING_FORMAT {
        return Err(invalid(format!(
            "{}: format {:?} is not {PENDING_FORMAT:?}",
            path.display(),
            pending.format
        )));
    }
    for id in pending.delete.iter().chain(&pending.remove) {
        iem_core::config::validate_member_id(id)
            .map_err(|e| invalid(format!("{}: {e}", path.display())))?;
    }
    for id in &pending.delete {
        for kind in KINDS {
            if let Err(e) = fs::remove_file(config_dir.join(file_of(kind, id)))
                && e.kind() != io::ErrorKind::NotFound
            {
                return Err(e);
            }
        }
    }
    let mut removed = Vec::new();
    for id in &pending.remove {
        if roster.person(id).is_none() && remove_person(config_dir, pins, id)? {
            tracing::info!(member = %id, "former member removed");
            removed.push(id.clone());
        }
    }
    fs::remove_file(path)?;
    Ok(removed)
}

#[cfg(test)]
mod tests;
