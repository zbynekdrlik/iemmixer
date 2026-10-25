//! Removing a person (#58). The owner's ruling on the live system
//! (2026-10-24) overruled the people archive of decision 6: a person the
//! roster no longer holds is removed, not archived and not hidden. Their
//! PIN hash, their photo and every lineup's files of theirs
//! (`lineups/<lineup>/<kind>/<id>.json`) are deleted and nothing of theirs
//! is kept anywhere, so a person added again starts fresh, like any new
//! person. Logs name ids only (P6).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use iem_core::roster::valid_lineup_id;

use crate::lineup_store::{KINDS, LINEUPS_DIR};
use crate::photo_store::PhotoStore;
use crate::pin_store::{ENGINEER_ID, PinStore};

fn check_person(id: &str) -> io::Result<()> {
    iem_core::config::validate_member_id(id)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    if id == ENGINEER_ID {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the engineer is never removed",
        ));
    }
    Ok(())
}

/// The lineup directories under `root` (the live `lineups/`): every
/// directory named like a lineup, so never `rosters.kept`.
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

/// Removes `id`: their PIN hash first (no login while their files go),
/// then their photo and every lineup's files of theirs. `false` when
/// nothing of theirs was found (nothing is written). A photo or file that
/// cannot be deleted is logged and the others are deleted all the same;
/// then the first such error is returned, so the caller names the person
/// as not removed.
pub fn remove_person(config_dir: &Path, pins: &mut PinStore, id: &str) -> io::Result<bool> {
    check_person(id)?;
    let pin = pins.remove_member(id)?;
    let mut failed: Option<io::Error> = None;
    let photo = match PhotoStore::new(config_dir).remove(id) {
        Ok(found) => found,
        Err(e) => {
            tracing::error!(person = %id, error = %e, "a removed person's photo was not deleted");
            failed = Some(e);
            false
        }
    };
    let mut files = 0usize;
    for (lineup, kind, path) in files_of(&config_dir.join(LINEUPS_DIR), id)? {
        match fs::remove_file(&path) {
            Ok(()) => files += 1,
            Err(e) => {
                tracing::error!(
                    person = %id,
                    lineup = %lineup,
                    kind,
                    error = %e,
                    "a removed person's file was not deleted"
                );
                if failed.is_none() {
                    failed = Some(e);
                }
            }
        }
    }
    if let Some(e) = failed {
        return Err(e);
    }
    let removed = pin || photo || files > 0;
    if removed {
        tracing::info!(person = %id, pin, photo, files, "removed: their data is deleted");
    }
    Ok(removed)
}
