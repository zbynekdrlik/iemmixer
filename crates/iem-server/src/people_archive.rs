//! The people archive (#58 decision 6, design §9): a person who leaves is
//! removed from every live place and kept on disk for their return. Their
//! photo and every lineup's files of theirs move to `archive/<id>/`
//! (`photo.jpg`, `lineups/<lineup>/<kind>/<id>.json`), their PIN hash to
//! `pin_hashes.json`'s `archived` list (secrets stay in the secrets
//! directory). An archived person logs in nowhere and appears nowhere;
//! restoring brings every file back that has no newer one in its place.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use iem_core::roster::valid_lineup_id;

use crate::lineup_store::{KINDS, LINEUPS_DIR};
use crate::photo_store::PhotoStore;
use crate::pin_store::{ENGINEER_ID, PinStore};

/// `<config>/archive/`: one directory per archived person.
pub const ARCHIVE_DIR: &str = "archive";
const PHOTO: &str = "photo.jpg";

fn check_person(id: &str) -> io::Result<()> {
    iem_core::config::validate_member_id(id)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    if id == ENGINEER_ID {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the engineer is never archived",
        ));
    }
    Ok(())
}

/// The lineup directories under `root` (the live `lineups/` or an archived
/// person's): every directory named like a lineup.
fn lineup_dirs(root: &Path) -> io::Result<Vec<(String, PathBuf)>> {
    let entries = match fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if valid_lineup_id(&name) && entry.file_type()?.is_dir() {
            out.push((name, entry.path()));
        }
    }
    out.sort();
    Ok(out)
}

/// Every `(lineup, kind)` file of `id` under `root`.
fn files_of(root: &Path, id: &str) -> io::Result<Vec<(String, &'static str, PathBuf)>> {
    let mut out = Vec::new();
    for (lineup, dir) in lineup_dirs(root)? {
        for kind in KINDS {
            let path = dir.join(kind).join(format!("{id}.json"));
            if path.try_exists()? {
                out.push((lineup.clone(), kind, path));
            }
        }
    }
    Ok(out)
}

fn move_file(from: &Path, to: &Path) -> io::Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(from, to)
}

/// Removes `dir` and the directories in it that are empty; anything with
/// a file left stays.
fn prune(dir: &Path) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            prune(&entry.path())?;
        }
    }
    match fs::remove_dir(dir) {
        Err(e) if e.kind() != io::ErrorKind::DirectoryNotEmpty => Err(e),
        _ => Ok(()),
    }
}

/// Archives `id`: their photo, every lineup's files of theirs and their PIN
/// hash. `false` when nothing of theirs was found (nothing is created).
pub fn archive_person(config_dir: &Path, pins: &mut PinStore, id: &str) -> io::Result<bool> {
    check_person(id)?;
    let home = config_dir.join(ARCHIVE_DIR).join(id);
    let mut archived = PhotoStore::new(config_dir).move_to(id, &home.join(PHOTO))?;
    for (lineup, kind, path) in files_of(&config_dir.join(LINEUPS_DIR), id)? {
        let to = home
            .join(LINEUPS_DIR)
            .join(lineup)
            .join(kind)
            .join(format!("{id}.json"));
        move_file(&path, &to)?;
        archived = true;
    }
    archived |= pins.archive_member(id)?;
    if archived {
        tracing::info!(person = %id, "archived");
    }
    Ok(archived)
}

/// Brings `id` back from the archive: the photo, each lineup's files and the
/// PIN hash, each unless a newer one took its place (that one is kept, the
/// archived one stays in the archive). `true` when anything came back.
pub fn restore_person(config_dir: &Path, pins: &mut PinStore, id: &str) -> io::Result<bool> {
    check_person(id)?;
    let home = config_dir.join(ARCHIVE_DIR).join(id);
    let mut restored = false;
    if home.try_exists()? {
        restored |= PhotoStore::new(config_dir).restore_from(id, &home.join(PHOTO))?;
        for (lineup, kind, path) in files_of(&home.join(LINEUPS_DIR), id)? {
            let to = config_dir
                .join(LINEUPS_DIR)
                .join(lineup)
                .join(kind)
                .join(format!("{id}.json"));
            if !to.try_exists()? {
                move_file(&path, &to)?;
                restored = true;
            }
        }
        prune(&home)?;
    }
    restored |= pins.restore_member(id)?;
    if restored {
        tracing::info!(person = %id, "restored from the archive");
    }
    Ok(restored)
}

#[cfg(test)]
mod tests;
