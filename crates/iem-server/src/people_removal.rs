//! Removing a person (#58). The owner's ruling on the live system
//! (2026-10-24) overruled the people archive of decision 6: a person the
//! roster no longer holds is removed, not archived and not hidden. Their
//! PIN hash, their photo and every lineup's files of theirs
//! (`lineups/<lineup>/<kind>/<id>.json`) are deleted and nothing of theirs
//! is kept anywhere, so a person added again starts fresh, like any new
//! person. What a removal could not delete is found again at the next
//! start or reload ([`held_ids`]). Logs name ids only (P6).

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use iem_core::roster::{valid_lineup_id, valid_person_id};

use crate::lineup_store::{KINDS, LINEUPS_DIR, retrying};
use crate::photo_store::{PHOTO_EXT, PHOTOS_DIR, PhotoStore};
use crate::pin_store::PinStore;

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

/// Adds to `out` the `<stem>` of every `<stem>.<ext>` in `dir` (none when
/// it does not exist) that is a lower-case person id; any other name (a
/// temporary file, one an upper-case id holds: on NTFS a person's own) is
/// never taken for anyone's.
fn stems(dir: &Path, ext: &str, out: &mut BTreeSet<String>) -> io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let name = entry?.file_name();
        let Some(stem) = name
            .to_str()
            .and_then(|n| n.strip_suffix(ext))
            .and_then(|n| n.strip_suffix('.'))
        else {
            continue;
        };
        if valid_person_id(stem) {
            out.insert(stem.to_string());
        }
    }
    Ok(())
}

/// Every id whose photo (`photos/<id>.jpg`) or lineup file
/// (`lineups/<lineup>/<kind>/<id>.json`) is on disk, lower-case person ids
/// only, in id order: what the start and every reload look through, so a
/// removal that failed part-way, or a person removed from the roster while
/// the server was stopped, is deleted all the same (#58 review).
pub fn held_ids(config_dir: &Path) -> io::Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    stems(&config_dir.join(PHOTOS_DIR), PHOTO_EXT, &mut out)?;
    for (_, dir) in lineup_dirs(&config_dir.join(LINEUPS_DIR))? {
        for kind in KINDS {
            stems(&dir.join(kind), "json", &mut out)?;
        }
    }
    Ok(out)
}

/// `<config>/archive/`: the people archive an older server wrote (#58
/// decision 6, overruled by the owner on 2026-10-24). Never written or
/// read, only deleted ([`drop_archive`]).
pub const ARCHIVE_DIR: &str = "archive";

/// Deletes the `archive/` folder an older server wrote, whatever it holds,
/// tried again on a short-lived Windows error; `false` when there is none
/// (a file of that name is no archive and stays).
pub fn drop_archive(config_dir: &Path) -> io::Result<bool> {
    let dir = config_dir.join(ARCHIVE_DIR);
    if !dir.is_dir() {
        return Ok(false);
    }
    retrying(cfg!(windows), || fs::remove_dir_all(&dir))?;
    tracing::info!("the people archive an older server wrote is deleted");
    Ok(true)
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
/// nothing of theirs was found (nothing is written). An id that is no
/// lower-case person id (`roster::valid_person_id`; on case-insensitive
/// NTFS `Member3`'s files are `member3`'s), or the engineer (by the PIN
/// store), is refused (`InvalidInput`, the id not echoed) before anything
/// is deleted. A photo or file that cannot be deleted is logged and the
/// others are deleted all the same; then the first such error is returned,
/// so the caller names the person as not removed. Each delete is tried
/// again on a short-lived Windows error, as a lineup file's rename is
/// (`lineup_store::retrying`: a scanner or the indexer holding the file);
/// what is left after that is deleted by the next start or reload
/// (`roster_watch::follow_people` looks through [`held_ids`]).
pub fn remove_person(config_dir: &Path, pins: &mut PinStore, id: &str) -> io::Result<bool> {
    remove_with(config_dir, pins, id, cfg!(windows), &|path| {
        fs::remove_file(path)
    })
}

/// [`remove_person`], each photo and file deleted by `remove`, tried
/// again while it fails with a transient error (`windows`: the server's
/// OS; tests pass `true` with a `remove` that fails as Windows does).
pub(crate) fn remove_with(
    config_dir: &Path,
    pins: &mut PinStore,
    id: &str,
    windows: bool,
    remove: &dyn Fn(&Path) -> io::Result<()>,
) -> io::Result<bool> {
    if !valid_person_id(id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a removal takes a lower-case person id only",
        ));
    }
    let delete = |path: &Path| retrying(windows, || remove(path));
    let pin = pins.remove_member(id)?;
    let mut failed: Option<io::Error> = None;
    let photo = match PhotoStore::new(config_dir).remove(id, &delete) {
        Ok(found) => found,
        Err(e) => {
            tracing::error!(person = %id, error = %e, "a removed person's photo was not deleted");
            failed = Some(e);
            false
        }
    };
    let mut files = 0usize;
    for (lineup, kind, path) in files_of(&config_dir.join(LINEUPS_DIR), id)? {
        match delete(&path) {
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

#[cfg(test)]
mod tests;
